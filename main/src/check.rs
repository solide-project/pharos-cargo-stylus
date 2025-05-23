// Copyright 2023-2024, Offchain Labs, Inc.
// For licensing, see https://github.com/OffchainLabs/cargo-stylus/blob/main/licenses/COPYRIGHT.md

use crate::{
    constants::TOOLCHAIN_FILE_NAME,
    macros::*,
    project::{self, extract_toolchain_channel, BuildConfig},
    util::color::Color,
    CheckConfig,
    export_abi::{self},
};
use ethers::{
    types::{
        U256 as EU256,
    },
};
use alloy_primitives::U256;
use bytesize::ByteSize;
use eyre::{eyre, ErrReport, Result, WrapErr};
use std::path::PathBuf;

/// Checks that a contract is valid and can be deployed onchain.
/// Returns whether the WASM is already up-to-date and activated onchain, and the data fee.
pub async fn check(cfg: &CheckConfig) -> Result<ContractCheck> {
    let verbose = cfg.common_cfg.verbose;
    let (wasm, project_hash) = cfg.build_wasm().wrap_err("failed to build wasm")?;

    if verbose {
        greyln!("reading wasm file at {}", wasm.to_string_lossy().lavender());
    }

    if let Err(e) = export_abi::export_abi(None, true) {
        eprintln!("Error: {:?}", e);
    }

    let (wasm_file_bytes, code) =
        project::compress_wasm(&wasm, project_hash).wrap_err("failed to compress WASM")?;

    let init_code = contract_deployment_calldata(&code);
    let deploy_code: String = init_code
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect();

    println!("DEPLOYMENT_CODE: {}", deploy_code);

    if verbose {
        greyln!("connecting to RPC: {}", &cfg.common_cfg.endpoint.lavender());
    }

    Ok(ContractCheck::Ready {
        code: wasm_file_bytes,
        fee: U256::from(0_u32),
    })
}

/// Whether a contract is active, or needs activation.
#[derive(PartialEq)]
pub enum ContractCheck {
    /// Contract can be activated with the given data fee.
    Ready { code: Vec<u8>, fee: U256 },
}

impl ContractCheck {
    pub fn code(&self) -> &[u8] {
        match self {
            Self::Ready { code, .. } => code,
        }
    }
    pub fn suggest_fee(&self) -> U256 {
        match self {
            Self::Ready { fee, .. } => *fee,
        }
    }
}

impl CheckConfig {
    fn build_wasm(&self) -> Result<(PathBuf, [u8; 32])> {
        if let Some(wasm) = self.wasm_file.clone() {
            return Ok((wasm, [0u8; 32]));
        }
        let toolchain_file_path = PathBuf::from(".").as_path().join(TOOLCHAIN_FILE_NAME);
        let toolchain_channel = extract_toolchain_channel(&toolchain_file_path)?;
        let rust_stable = !toolchain_channel.contains("nightly");
        let mut cfg = BuildConfig::new(rust_stable);
        cfg.features = self.common_cfg.features.clone();
        let wasm = project::build_dylib(cfg.clone())?;
        let project_hash =
            project::hash_project(self.common_cfg.source_files_for_project_hash.clone(), cfg)?;
        Ok((wasm, project_hash))
    }
}

/// Pretty-prints a file size based on its limits.
pub fn format_file_size(len: usize, mid: u64, max: u64) -> String {
    let len = ByteSize::b(len as u64);
    let mid = ByteSize::kib(mid);
    let max = ByteSize::kib(max);
    if len <= mid {
        len.mint()
    } else if len <= max {
        len.yellow()
    } else {
        len.pink()
    }
}

pub struct EthCallError {
    pub data: Vec<u8>,
    pub msg: String,
}

impl From<EthCallError> for ErrReport {
    fn from(value: EthCallError) -> Self {
        eyre!(value.msg)
    }
}

pub fn contract_deployment_calldata(code: &[u8]) -> Vec<u8> {
    let mut code_len = [0u8; 32];
    EU256::from(code.len()).to_big_endian(&mut code_len);
    let mut deploy: Vec<u8> = vec![];
    deploy.push(0x7f); // PUSH32
    deploy.extend(code_len);
    deploy.push(0x80); // DUP1
    deploy.push(0x60); // PUSH1
    deploy.push(42 + 1); // prelude + version
    deploy.push(0x60); // PUSH1
    deploy.push(0x00);
    deploy.push(0x39); // CODECOPY
    deploy.push(0x60); // PUSH1
    deploy.push(0x00);
    deploy.push(0xf3); // RETURN
    deploy.push(0x00); // version
    deploy.extend(code);
    deploy
}