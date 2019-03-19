use std::collections::HashMap;

use super::wallet::WalletInfo;
use sdag_wallet_base::Base64KeyExt;

use sdag::error::Result;
use sdag::object_hash;
use sdag::{config, joint::Joint, spec::*};

pub const GENESIS_FILE: &str = "genesis.json";
pub const FIRST_PAYMENT: &str = "first_payment.json";
pub const INIT_MNEMONIC: &str = "init_mnemonic.json";

const WITNESSES_NUM: usize = 12;

pub struct SdagInitInfo {
    pub witnesses: [WalletInfo; WITNESSES_NUM],
    pub sdag_org: WalletInfo,
}

pub fn gen_all_wallets() -> Result<SdagInitInfo> {
    Ok(SdagInitInfo {
        witnesses: [
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
            WalletInfo::from_mnemonic("")?,
        ],
        sdag_org: WalletInfo::from_mnemonic("")?,
    })
}

// generate genesis unit according to the params
pub fn gen_genesis_joint(wallets: &SdagInitInfo, total: u64, msg: &str) -> Result<Joint> {
    let mut witnesses = wallets
        .witnesses
        .iter()
        .map(|s| s._00_address.clone())
        .collect::<Vec<String>>();

    witnesses.sort();

    // preare a defaut unit first
    let mut unit = Unit {
        messages: vec![sdag::composer::create_text_message(&String::from(msg))?],
        earned_headers_commission_recipients: vec![HeaderCommissionShare {
            // give the header commission to sdag_org
            address: wallets.sdag_org._00_address.clone(),
            earned_headers_commission_share: 100,
        }],
        main_chain_index: Some(0),
        ..Default::default()
    };

    // prepare outputs
    let amount = 1_000_000;
    let mut outputs = Vec::new();
    // for witness multi utxo
    for address in witnesses.iter() {
        for _i in 0..8 {
            outputs.push(Output {
                address: address.clone(),
                amount,
            });
        }
    }
    // change output
    outputs.push(Output {
        address: wallets.sdag_org._00_address.clone(),
        amount: 0,
    });

    outputs.sort_by(|a, b| a.address.cmp(&b.address));

    // prepare payment message
    let payment_message = Message {
        app: "payment".to_string(),
        payload_location: "inline".to_string(),
        // use dummpy hash to calc the correct payload size
        payload_hash: "-".repeat(config::HASH_LENGTH),
        payload: Some(Payload::Payment(Payment {
            address: None,
            asset: None,
            definition_chash: None,
            denomination: None,
            inputs: vec![Input {
                kind: Some(String::from("issue")),
                serial_number: Some(1),
                amount: Some(total),
                address: Some(wallets.witnesses[0]._00_address.clone()),
                ..Default::default()
            }],
            outputs,
        })),
        payload_uri: None,
        payload_uri_hash: None,
        spend_proofs: Vec::new(),
    };

    // messges
    unit.messages.push(payment_message);

    // authors
    for from_address in &wallets.witnesses {
        let author = Author {
            address: from_address._00_address.clone(),
            authentifiers: {
                // here we use a dummy signature to calc the correct header size
                let mut sign = HashMap::new();
                sign.insert("r".to_string(), "-".repeat(config::SIG_LENGTH));
                sign
            },
            definition: json!([
                "sig",
                {
                    "pubkey": from_address._00_address_pubk.to_base64_key()
                }
            ]),
        };
        unit.authors.push(author);
        unit.authors.sort_by(|a, b| a.address.cmp(&b.address));
    }

    // witnesses
    unit.witnesses = witnesses;
    //unit.witnesses = witnesses.sort();
    // input coins
    unit.headers_commission = Some(unit.calc_header_size());
    unit.payload_commission = Some(unit.calc_payload_size());

    {
        let payment_message = unit.messages.last_mut().unwrap();

        let foundation_amount = total
            - (amount as usize * WITNESSES_NUM * 8) as u64
            - unit.headers_commission.unwrap() as u64
            - unit.payload_commission.unwrap() as u64;

        match payment_message.payload {
            Some(Payload::Payment(ref mut x)) => {
                for output in x.outputs.iter_mut() {
                    if output.address == wallets.sdag_org._00_address {
                        output.amount = foundation_amount;
                    }
                }
                payment_message.payload_hash = object_hash::get_base64_hash(&x)?;
            }
            _ => {}
        }
    }

    // fix the authentifiers
    let unit_hash = unit.calc_unit_hash_to_sign();
    for author in &mut unit.authors {
        if let Some(ref index) = wallets
            .witnesses
            .iter()
            .find(|ref probe| probe._00_address == author.address)
        {
            let signature = sdag_wallet_base::sign(&unit_hash, &index._00_address_prvk)?;
            author.authentifiers.insert("r".to_string(), signature);
        }
    }

    unit.timestamp = Some(::sdag::time::now() / 1000);
    unit.unit = unit.calc_unit_hash();

    Ok(Joint {
        ball: Some(object_hash::calc_ball_hash(
            &unit.calc_unit_hash(),
            &Vec::new(),
            &Vec::new(),
            false,
        )),
        skiplist_units: Vec::new(),
        unsigned: None,
        unit,
    })
}

pub fn gen_first_payment(
    paying_wallet: &WalletInfo,
    address_amount: u64,
    genesis_joint: &Joint,
) -> Result<Joint> {
    // preare a defaut unit first
    let mut unit = Unit {
        messages: vec![],
        earned_headers_commission_recipients: vec![HeaderCommissionShare {
            // give the header commission to sdag_org
            address: paying_wallet._00_address.clone(),
            earned_headers_commission_share: 100,
        }],
        main_chain_index: None,
        ..Default::default()
    };

    // prepare outputs
    let mut outputs = Vec::new();
    let first_wallet = WalletInfo::from_mnemonic("")?;

    outputs.push(Output {
        address: first_wallet._00_address.clone(),
        amount: address_amount,
    });

    // change output
    outputs.push(Output {
        address: paying_wallet._00_address.clone(),
        amount: 0,
    });
    outputs.sort_by(|a, b| a.address.cmp(&b.address));

    let foundation_total_amount: i64 = 499_999_903_993_426;

    let mut index = 0;
    for message in &genesis_joint.unit.messages {
        match &message.payload {
            Some(Payload::Payment(x)) => {
                for output in &x.outputs {
                    if output.address == paying_wallet._00_address {
                        break;
                    }
                    index += 1;
                }
            }
            _ => {}
        }
    }

    // prepare payment message
    let payment_message = Message {
        app: "payment".to_string(),
        payload_location: "inline".to_string(),
        // use dummpy hash to calc the correct payload size
        payload_hash: "-".repeat(config::HASH_LENGTH),
        payload: Some(Payload::Payment(Payment {
            address: None,
            asset: None,
            definition_chash: None,
            denomination: None,
            inputs: vec![Input {
                unit: Some(genesis_joint.unit.unit.clone()),
                message_index: Some(1),
                output_index: Some(index as u32),
                ..Default::default()
            }],
            outputs,
        })),
        payload_uri: None,
        payload_uri_hash: None,
        spend_proofs: Vec::new(),
    };

    // messges
    unit.messages.push(payment_message);
    unit.parent_units = vec![genesis_joint.unit.unit.clone()];
    unit.last_ball = genesis_joint.ball.clone();
    unit.last_ball_unit = Some(genesis_joint.unit.unit.clone());
    // authors
    unit.authors.push(Author {
        address: paying_wallet._00_address.clone(),
        authentifiers: {
            // here we use a dummy signature to calc the correct header size
            let mut sign = HashMap::new();
            sign.insert("r".to_string(), "-".repeat(config::SIG_LENGTH));
            sign
        },
        definition: json!([
            "sig",
            {
                "pubkey": paying_wallet._00_address_pubk.to_base64_key()
            }
        ]),
    });
    unit.authors.sort_by(|a, b| a.address.cmp(&b.address));

    // witnesses
    unit.witness_list_unit = Some(genesis_joint.unit.unit.clone());

    unit.headers_commission = Some(unit.calc_header_size());
    unit.payload_commission = Some(unit.calc_payload_size());

    {
        let payment_message = unit.messages.last_mut().unwrap();

        let foundation_amount = foundation_total_amount as u64
            - address_amount as u64
            - unit.headers_commission.unwrap() as u64
            - unit.payload_commission.unwrap() as u64;

        match payment_message.payload {
            Some(Payload::Payment(ref mut x)) => {
                for output in x.outputs.iter_mut() {
                    if output.address == paying_wallet._00_address {
                        output.amount = foundation_amount;
                    }
                }
                payment_message.payload_hash = object_hash::get_base64_hash(&x)?;
            }
            _ => {}
        }
    }

    // fix the authentifiers
    let unit_hash = unit.calc_unit_hash_to_sign();

    for author in &mut unit.authors {
        let signature = sdag_wallet_base::sign(&unit_hash, &paying_wallet._00_address_prvk)?;
        author.authentifiers.insert("r".to_string(), signature);
    }

    unit.timestamp = Some(::sdag::time::now() / 1000);
    unit.unit = unit.calc_unit_hash();
    Ok(Joint {
        ball: None,
        skiplist_units: Vec::new(),
        unsigned: None,
        unit,
    })
}
