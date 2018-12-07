#[macro_use]
extern crate log;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate serde_derive;

extern crate chrono;
extern crate fern;
extern crate may;
extern crate sdag;
extern crate sdag_wallet_base;
extern crate serde;
extern crate serde_json;

mod config;

use chrono::{Local, TimeZone};
use clap::App;
use failure::ResultExt;
use spec::Output;
use std::sync::Arc;

use composer::*;
use light::*;
use sdag::network::wallet::WalletConn;
use sdag::signature::Signer;
use sdag::*;
use sdag_wallet_base::{Base64KeyExt, ExtendedPrivKey, ExtendedPubKey, Mnemonic};

struct WalletInfo {
    #[allow(dead_code)]
    master_prvk: ExtendedPrivKey,
    wallet_pubk: ExtendedPubKey,
    device_address: String,
    wallet_0_id: String,
    _00_address: String,
    _00_address_pubk: ExtendedPubKey,
    _00_address_prvk: ExtendedPrivKey,
}

impl WalletInfo {
    fn from_mnemonic(mnemonic: &str) -> Result<WalletInfo> {
        let wallet = 0;
        let mnemonic = Mnemonic::from(&mnemonic)?;
        let master_prvk = sdag_wallet_base::master_private_key(&mnemonic, "")?;
        let device_address = sdag_wallet_base::device_address(&master_prvk)?;
        let wallet_pubk = sdag_wallet_base::wallet_pubkey(&master_prvk, wallet)?;
        let wallet_0_id = sdag_wallet_base::wallet_id(&wallet_pubk);
        let _00_address = sdag_wallet_base::wallet_address(&wallet_pubk, false, 0)?;
        let _00_address_prvk = sdag_wallet_base::wallet_address_prvkey(&master_prvk, 0, false, 0)?;
        let _00_address_pubk = sdag_wallet_base::wallet_address_pubkey(&wallet_pubk, false, 0)?;

        Ok(WalletInfo {
            master_prvk,
            wallet_pubk,
            device_address,
            wallet_0_id,
            _00_address,
            _00_address_pubk,
            _00_address_prvk,
        })
    }
}

impl Signer for WalletInfo {
    fn sign(&self, hash: &[u8], address: &str) -> Result<String> {
        if address != self._00_address {
            bail!("invalid address for wallet to sign");
        }

        sdag_wallet_base::sign(hash, &self._00_address_prvk)
    }
}

fn init_log(verbosity: u64) {
    let log_lvl = match verbosity {
        0 => log::LevelFilter::Off,
        1 => log::LevelFilter::Error,
        2 => log::LevelFilter::Info,
        _ => log::LevelFilter::Debug,
    };

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{}[{}][{}] {}",
                chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S%.3f]"),
                record.level(),
                record.target(),
                message
            ))
        })
        .level(log_lvl)
        .chain(std::io::stdout())
        .apply()
        .unwrap();

    debug!("log init done!");
}

fn init(verbosity: u64) -> Result<()> {
    // init default coroutine settings
    let stack_size = if cfg!(debug_assertions) {
        0x4000
    } else {
        0x2000
    };
    may::config().set_stack_size(stack_size);

    init_log(verbosity);

    Ok(())
}

fn connect_to_remote(peers: &[String]) -> Result<Arc<WalletConn>> {
    for peer in peers {
        match network::wallet::create_outbound_conn(&peer) {
            Err(e) => {
                error!(" fail to connected: {}, err={}", peer, e);
                continue;
            }
            Ok(c) => return Ok(c),
        }
    }
    bail!("failed to connect remote hub");
}

fn info(ws: &Arc<WalletConn>, wallet_info: &WalletInfo) -> Result<()> {
    let address_pubk = wallet_info._00_address_pubk.to_base64_key();

    let stable = ws.get_balance(&wallet_info._00_address)? as f64 / 1000_000.0;

    println!("\ncurrent wallet info:\n");
    println!("device_address: {}", wallet_info.device_address);
    println!("wallet_public_key: {}", wallet_info.wallet_pubk.to_string());
    println!("└──wallet_id(0): {}", wallet_info.wallet_0_id);
    println!("   └──address(0/0): {}", wallet_info._00_address);
    println!("      ├── path: /m/44'/0'/0'/0/0");
    println!("      ├── pubkey: {}", address_pubk);
    println!("      └── balance: {:.6}", stable);

    Ok(())
}

fn show_history(
    ws: &Arc<WalletConn>,
    address: &String,
    index: Option<usize>,
    num: usize,
) -> Result<()> {
    let history = ws.get_latest_history(address.clone(), num)?;

    if let Some(index) = index {
        // show special unit's detail information
        if index == 0 || index > history.transactions.len() {
            bail!("invalid transaction index");
        }

        let history = &history.transactions[index - 1];
        let mut amount;
        if &history.to_addr == address {
            println!("FROM     : {}", history.from_addr);
            amount = history.amount;
        } else {
            println!("TO       : {}", history.to_addr);
            amount = 0 - history.amount;
        }
        println!("UNIT     : {}", history.unit_hash);
        println!("AMOUNT   : {:.6} MN", amount as f64 / 1_000_000.0);
        println!(
            "DATE     : {}",
            Local
                .timestamp(history.time.unwrap_or(0) as i64, 0)
                .naive_local()
        );
    } else {
        for (id, transaction) in history.transactions.iter().enumerate() {
            if id > num - 1 {
                break;
            }
            let amount = if &transaction.to_addr == address {
                transaction.amount
            } else {
                0 - transaction.amount
            };

            println!(
                "#{:<4} {:>10.6} MN  \t{}",
                id + 1,
                amount as f64 / 1_000_000.0,
                Local
                    .timestamp(transaction.time.unwrap_or(0) as i64, 0)
                    .naive_local()
            );
        }
    }

    Ok(())
}

fn send_payment(
    ws: &Arc<WalletConn>,
    text: Option<String>,
    address_amount: Vec<(String, f64)>,
    wallet_info: &WalletInfo,
) -> Result<()> {
    let messages = if text.is_some() {
        vec![composer::create_text_message(text.as_ref().unwrap())?]
    } else {
        vec![]
    };
    let light_props = ws.get_light_props(&wallet_info._00_address)?;

    let outputs = address_amount
        .iter()
        .map(|(address, amount)| Output {
            address: address.clone(),
            amount: (amount * 1_000_000.0).round() as u64,
        })
        .collect::<Vec<_>>();

    let total_amount = outputs.iter().fold(0, |acc, x| acc + x.amount);

    let compose_info = ComposeInfo {
        paid_address: wallet_info._00_address.clone(),
        change_address: wallet_info._00_address.clone(),
        outputs,
        messages,
        transaction_amount: total_amount,
        is_spend_all: false,
        light_props,
        pubk: wallet_info._00_address_pubk.to_base64_key(),
    };

    let unit = compose_naked_joint(compose_info.clone())?;

    let input_response: InputsResponse =
        ws.get_inputs_from_hub(&unit, total_amount, compose_info.is_spend_all)?;

    let joint = compose_joint(unit, input_response, compose_info, wallet_info)?;

    if let Err(e) = ws.post_joint(&joint) {
        eprintln!("post_joint err={}", e);
        return Err(e);
    }

    println!("FROM  : {}", wallet_info._00_address);
    println!("TO    : ");
    for (address, amount) in address_amount {
        println!("      address : {}, amount : {}", address, amount);
    }
    println!("UNIT  : {}", joint.unit.unit);

    if text.is_some() {
        println!("TEXT  : {}", text.unwrap_or("".to_string()));
    }

    println!(
        "DATE  : {}",
        Local.timestamp_millis(time::now() as i64).naive_local()
    );

    Ok(())
}

fn main() -> Result<()> {
    let yml = load_yaml!("ttt.yml");
    let m = App::from_yaml(yml).get_matches();

    let verbosity = m.occurrences_of("verbose");
    init(verbosity)?;

    // init command
    if let Some(init_arg) = m.subcommand_matches("init") {
        if let Some(mnemonic) = init_arg.value_of("MNEMONIC") {
            config::update_mnemonic(mnemonic)?;
        }
        // create settings
        let settings = config::get_settings();
        settings.show_config();
        // every init would remove the local database
        ::std::fs::remove_file(sdag::config::get_database_path(true)).ok();
        return Ok(());
    }

    let settings = config::get_settings();
    let ws = connect_to_remote(&settings.hub_url)?;

    //raw_post
    if let Some(raw_post) = m.subcommand_matches("raw_post") {
        if let Some(file) = raw_post.value_of("JOINT_FILE") {
            println!("raw_post file = {}", file);

            let file = ::std::fs::File::open(file)?;
            let joint = serde_json::from_reader(file)?;

            println!("joint = {:#?}", joint);
            ws.post_joint(&joint)?;
            return Ok(());
        }
        unreachable!("must have a joint json file");
    }

    let wallet_info = WalletInfo::from_mnemonic(&settings.mnemonic)?;

    //info
    if let Some(_info) = m.subcommand_matches("info") {
        return info(&ws, &wallet_info);
    }

    //Log
    if let Some(log) = m.subcommand_matches("log") {
        let index = value_t!(log.value_of("v"), usize).ok();

        match value_t!(log.value_of("n"), usize) {
            Ok(num) => {
                return show_history(&ws, &wallet_info._00_address, index, num);
            }
            Err(clap::Error {
                kind: clap::ErrorKind::ArgumentNotFound,
                ..
            }) => {
                return show_history(&ws, &wallet_info._00_address, index, 5);
            }
            Err(e) => e.exit(),
        }
    }

    //Send
    if let Some(send) = m.subcommand_matches("send") {
        let mut address_amount = Vec::new();
        if let Some(pay) = send.values_of("pay") {
            let v = pay.collect::<Vec<_>>();
            for arg in v.chunks(2) {
                if !::object_hash::is_chash_valid(arg[0]) {
                    eprintln!("invalid address, please check");
                    return Ok(());
                }
                let amount = arg[1].parse::<f64>().context("invalid amount arg")?;
                if amount > std::u64::MAX as f64 || amount < 0.000001 {
                    eprintln!("invalid amount, please check");
                    return Ok(());
                }
                address_amount.push((arg[0].to_string(), amount));
            }
        }

        if let Some(text) = send.value_of("text") {
            return send_payment(&ws, Some(text.to_string()), address_amount, &wallet_info);
        } else {
            return send_payment(&ws, None, address_amount, &wallet_info);
        }
    }

    if let Some(balance) = m.subcommand_matches("balance") {
        if let Some(_s) = balance.values_of("s") {
            println!(
                "{:.6}",
                ws.get_balance(&wallet_info._00_address)? as f64 / 1000_000.0
            );
            return Ok(());
        }

        return Ok(());
    }

    Ok(())
}
