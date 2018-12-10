use std::collections::HashMap;

use cache::SDAG_CACHE;
use config;
use error::Result;
use joint::Joint;
use light::*;
use object_hash;
use serde_json::Value;
use signature::Signer;
use spec::*;

#[derive(Serialize, Deserialize)]
pub struct ParentsAndLastBall {
    pub parents: Vec<String>,
    pub last_ball: String,
    pub last_ball_unit: String,
}

#[derive(Serialize, Deserialize)]
pub struct ComposeInfo {
    pub paid_address: String,
    pub change_address: String,
    pub outputs: Vec<Output>,
    pub inputs: InputsResponse,
    pub transaction_amount: u64,
    pub text_message: Option<Message>,
    pub light_props: LightProps,
    pub pubk: String,
}

pub fn pick_parents_and_last_ball(_address: &str) -> Result<ParentsAndLastBall> {
    let free_joints = SDAG_CACHE.get_free_joints()?;
    let last_stable_joint = ::main_chain::get_last_stable_joint();

    for group in free_joints.chunks(config::MAX_PARENT_PER_UNIT) {
        if ::main_chain::is_stable_in_later_joints(&last_stable_joint, &group)? {
            let mut parents = group.iter().map(|p| p.key.to_string()).collect::<Vec<_>>();
            parents.sort();
            let lsj_data = last_stable_joint.read()?;

            return Ok(ParentsAndLastBall {
                parents,
                last_ball: lsj_data.ball.clone().expect("ball in joint is none"),
                last_ball_unit: lsj_data.unit.unit.clone(),
            });
        }
    }

    bail!("fail to choose parents")
}

/// create a pure text message
pub fn create_text_message(text: &str) -> Result<Message> {
    Ok(Message {
        app: String::from("text"),
        payload_location: String::from("inline"),
        payload_hash: object_hash::get_base64_hash(text)?,
        payload: Some(Payload::Text(text.to_string())),
        ..Default::default()
    })
}

pub fn compose_joint<T: Signer>(composer_info: ComposeInfo, signer: &T) -> Result<Joint> {
    let ComposeInfo {
        paid_address,
        change_address,
        transaction_amount,
        mut outputs,
        mut inputs,
        light_props,
        text_message,
        pubk,
    } = composer_info;

    let mut unit = Unit {
        messages: text_message.into_iter().collect::<Vec<_>>(),
        ..Default::default()
    };

    unit.last_ball = Some(light_props.last_ball);
    unit.last_ball_unit = Some(light_props.last_ball_unit);
    unit.witness_list_unit = Some(light_props.witness_list_unit);
    unit.parent_units = light_props.parent_units;

    let definition = if light_props.has_definition {
        Value::Null
    } else {
        json!(["sig", { "pubkey": pubk }])
    };
    let authors = vec![Author {
        address: paid_address,
        authentifiers: {
            // here we use a dummy signature to calc the correct header size
            let mut sign = HashMap::new();
            sign.insert("r".to_string(), "-".repeat(config::SIG_LENGTH));
            sign
        },
        definition,
    }];

    unit.authors = authors;

    outputs.push(Output {
        address: change_address.clone(),
        amount: 0,
    });

    let payment_message = Message {
        app: "payment".to_string(),
        payload_location: "inline".to_string(),
        payload_hash: "-".repeat(config::HASH_LENGTH),
        payload: Some(Payload::Payment(Payment {
            address: None,
            asset: None,
            definition_chash: None,
            denomination: None,
            inputs: vec![],
            outputs: outputs,
        })),
        payload_uri: None,
        payload_uri_hash: None,
        spend_proofs: vec![],
    };

    unit.messages.push(payment_message);
    unit.headers_commission = Some(unit.calc_header_size());

    match unit.messages.last_mut().unwrap().payload {
        Some(Payload::Payment(ref mut x)) => {
            x.inputs.append(&mut inputs.inputs);
        }
        _ => {}
    }

    unit.payload_commission = Some(unit.calc_payload_size());
    info!(
        "inputs increased payload by {}",
        unit.payload_commission.unwrap()
    );

    let change = inputs.amount as i64
        - transaction_amount as i64
        - unit.headers_commission.unwrap() as i64
        - unit.payload_commission.unwrap() as i64;

    if change < 0 {
        bail!(
            "NOT_ENOUGH_FUNDS: address {} not enough spendable funds for fees",
            unit.authors[0].address
        );
    }

    {
        let payment_message = unit.messages.last_mut().unwrap();
        match payment_message.payload {
            Some(Payload::Payment(ref mut x)) => {
                for output in x.outputs.iter_mut() {
                    if change_address == output.address {
                        output.amount = change as u64;
                    }
                }

                x.outputs.sort_by(|a, b| {
                    if a.address == b.address {
                        a.amount.cmp(&b.amount)
                    } else {
                        a.address.cmp(&b.address)
                    }
                });

                payment_message.payload_hash = object_hash::get_base64_hash(&x)?;
            }
            _ => {}
        }
    }

    let unit_hash = unit.calc_unit_hash_to_sign();
    for mut author in &mut unit.authors {
        let signature = signer.sign(&unit_hash, &author.address)?;
        author.authentifiers.insert("r".to_string(), signature);
    }

    unit.timestamp = Some(::time::now() / 1000);
    unit.unit = unit.calc_unit_hash();

    Ok(Joint {
        ball: None,
        skiplist_units: Vec::new(),
        unsigned: None,
        unit,
    })
}
