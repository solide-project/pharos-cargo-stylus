// Copyright 2023-2024, Offchain Labs, Inc.
// For licensing, see https://github.com/OffchainLabs/cargo-stylus/blob/main/licenses/COPYRIGHT.md

#![allow(clippy::println_empty_string)]
use crate::{
    check, export_abi,
    macros::*,
    util::{
        color::{Color, DebugColor},
        sys,
    },
    DeployConfig,
};
use ethers::core::utils::format_units;
use ethers::{
    core::k256::ecdsa::SigningKey,
    middleware::SignerMiddleware,
    prelude::*,
    providers::{Middleware, Provider},
    signers::Signer,
    types::{transaction::eip2718::TypedTransaction, Eip1559TransactionRequest, H160, U256, U64},
};
use eyre::{bail, eyre, Result, WrapErr};

pub type SignerClient = SignerMiddleware<Provider<Http>, Wallet<SigningKey>>;

/// Deploys a stylus contract, activating if needed.
pub async fn deploy(cfg: DeployConfig) -> Result<()> {
    let contract = check::check(&cfg.check_config)
        .await
        .expect("cargo stylus check failed");
    let verbose = cfg.check_config.common_cfg.verbose;

    // Pharos call `deploy` function on the contract, do not call `constructor` through the system contract.
    let _constructor = export_abi::get_constructor_signature()?;

    let client = sys::new_provider(&cfg.check_config.common_cfg.endpoint)?;
    let chain_id = client.get_chainid().await.expect("failed to get chain id");

    let wallet = cfg.auth.wallet().wrap_err("failed to load wallet")?;
    let wallet = wallet.with_chain_id(chain_id.as_u64());
    let sender = wallet.address();
    let client = SignerMiddleware::new(client, wallet);

    if verbose {
        greyln!("sender address: {}", sender.debug_lavender());
    }

    let data_fee = contract.suggest_fee()
        + alloy_ethers_typecast::ethers_u256_to_alloy(cfg.experimental_constructor_value);

    // Check balance early
    let balance = client
        .get_balance(sender, None)
        .await
        .expect("failed to get balance");
    let balance = alloy_ethers_typecast::ethers_u256_to_alloy(balance);

    if balance < data_fee && !cfg.estimate_gas {
        bail!(
            "not enough funds in account {} to pay for data fee\n\
             balance {} < {}\n\
             please see the Quickstart guide for funding new accounts:\n{}",
            sender.red(),
            balance.red(),
            format!("{data_fee} wei").red(),
            "https://docs.pharosnetwork.xyz/developer-guides".yellow(),
        );
    }

    cfg.deploy_contract(contract.code(), sender, &client)
        .await?;

    if cfg.estimate_gas {
        return Ok(());
    }

    Ok(())
}

impl DeployConfig {
    async fn deploy_contract(
        &self,
        code: &[u8],
        sender: H160,
        client: &SignerClient,
    ) -> Result<H160> {
        let init_code = contract_deployment_calldata(code);

        let tx = Eip1559TransactionRequest::new()
            .from(sender)
            .data(init_code);

        let verbose = self.check_config.common_cfg.verbose;
        let gas = client
            .estimate_gas(&TypedTransaction::Eip1559(tx.clone()), None)
            .await?;

        if self.check_config.common_cfg.verbose || self.estimate_gas {
            print_gas_estimate("deployment", client, gas).await?;
        }
        if self.estimate_gas {
            let nonce = client.get_transaction_count(sender, None).await?;
            return Ok(ethers::utils::get_contract_address(sender, nonce));
        }

        let receipt = run_tx(
            "deploy",
            tx,
            Some(gas),
            self.check_config.common_cfg.max_fee_per_gas_gwei,
            client,
            self.check_config.common_cfg.verbose,
        )
        .await?;
        let contract = receipt.contract_address.ok_or(eyre!("missing address"))?;
        let address = contract.debug_lavender();

        if verbose {
            let gas = format_gas(receipt.gas_used.unwrap_or_default());
            greyln!(
                "deployed code at address: {address} {} {gas}",
                "with".grey()
            );
        } else {
            greyln!("deployed code at address: {address}");
        }
        let tx_hash = receipt.transaction_hash.debug_lavender();
        greyln!("deployment tx hash: {tx_hash}");
        Ok(contract)
    }
}

pub async fn print_gas_estimate(name: &str, client: &SignerClient, gas: U256) -> Result<()> {
    let gas_price = client.get_gas_price().await?;
    greyln!("estimates");
    greyln!("{} tx gas: {}", name, gas.debug_lavender());
    greyln!(
        "gas price: {} gwei",
        format_units(gas_price, "gwei")?.debug_lavender()
    );
    let total_cost = gas_price.checked_mul(gas).unwrap_or_default();
    let eth_estimate = format_units(total_cost, "ether")?;
    greyln!(
        "{} tx total cost: {} ETH",
        name,
        eth_estimate.debug_lavender()
    );
    Ok(())
}

pub async fn run_tx(
    name: &str,
    tx: Eip1559TransactionRequest,
    gas: Option<U256>,
    max_fee_per_gas_gwei: Option<u128>,
    client: &SignerClient,
    verbose: bool,
) -> Result<TransactionReceipt> {
    let mut tx = tx;
    if let Some(gas) = gas {
        tx.gas = Some(gas);
    }
    if let Some(max_fee) = max_fee_per_gas_gwei {
        tx.max_fee_per_gas = Some(U256::from(gwei_to_wei(max_fee)?));
    }
    let tx = TypedTransaction::Eip1559(tx);
    let tx = client.send_transaction(tx, None).await?;
    let tx_hash = tx.tx_hash();
    if verbose {
        greyln!("sent {name} tx: {}", tx_hash.debug_lavender());
    }
    let Some(receipt) = tx.await.wrap_err("tx failed to complete")? else {
        bail!("failed to get receipt for tx {}", tx_hash.lavender());
    };
    if receipt.status != Some(U64::from(1)) {
        bail!("{name} tx reverted {}", tx_hash.debug_red());
    }
    Ok(receipt)
}

/// Prepares an EVM bytecode prelude for contract creation.
pub fn contract_deployment_calldata(code: &[u8]) -> Vec<u8> {
    let mut code_len = [0u8; 32];
    U256::from(code.len()).to_big_endian(&mut code_len);
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

pub fn extract_contract_evm_deployment_prelude(calldata: &[u8]) -> Vec<u8> {
    // The length of the prelude, version part is 42 + 1 as per the code
    let metadata_length = 42 + 1;
    // Extract and return the metadata part
    calldata[0..metadata_length].to_vec()
}

pub fn extract_compressed_wasm(calldata: &[u8]) -> Vec<u8> {
    // The length of the prelude, version part is 42 + 1 as per the code
    let metadata_length = 42 + 1;
    // Extract and return the metadata part
    calldata[metadata_length..].to_vec()
}

pub fn format_gas(gas: U256) -> String {
    let gas: u64 = gas.try_into().unwrap_or(u64::MAX);
    let text = format!("{gas} gas");
    if gas <= 3_000_000 {
        text.mint()
    } else if gas <= 7_000_000 {
        text.yellow()
    } else {
        text.pink()
    }
}

pub fn gwei_to_wei(gwei: u128) -> Result<u128> {
    let wei_per_gwei: u128 = 10u128.pow(9);
    match gwei.checked_mul(wei_per_gwei) {
        Some(wei) => Ok(wei),
        None => bail!("overflow occurred while converting gwei to wei"),
    }
}
