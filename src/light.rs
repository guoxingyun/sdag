use error::Result;

use business::BUSINESS_CACHE;
use cache::SDAG_CACHE;
use spec::{Payload, Unit};

#[derive(Serialize, Deserialize)]
pub struct HistoryRequest {
    pub address: String,
    #[serde(default)]
    pub num: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub unit_hash: String,
    pub from_addr: String,
    pub to_addr: String,
    pub amount: u64,
    pub time: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub transaction_history: Vec<Transaction>,
}

#[derive(Serialize, Deserialize)]
pub struct InputsRequest {
    pub address: String,
    pub amount: u64,
    pub send_all: bool,
}

/// get history by address, return transactions
pub fn prepare_latest_history(history_request: &HistoryRequest) -> Result<HistoryResponse> {
    // note: just support get stable history currently
    // let mut unstable_txs = get_unstable_history(history_request, history_request.num);
    // let mut stable_txs = get_stable_history(history_request, limit - unstable_txs.len());
    // unstable_txs.append(&mut stable_txs);

    Ok(HistoryResponse {
        transaction_history: get_stable_history(history_request)?,
    })
}

/// get transactions from unstable joints
fn _get_unstable_history(
    _history_request: &HistoryRequest,
    _need_tx_count: usize,
) -> Vec<Transaction> {
    unimplemented!()
}

/// get transactions from stable joints
fn get_stable_history(history_request: &HistoryRequest) -> Result<Vec<Transaction>> {
    let address = &history_request.address;
    let num = history_request.num;

    let mut transactions = Vec::new();

    // receive money from others
    // history range (last_stable_self_joint, last_stable_joint]
    for unit in BUSINESS_CACHE.global_state.get_related_joints(address) {
        let related_joint_data = SDAG_CACHE.get_joint(&unit)?.read()?;
        if get_receive_tx(&related_joint_data.unit, address, num, &mut transactions) {
            return Ok(transactions);
        }
    }

    // history range (known_stable_self_units, last_stable_self_joint]
    // last_stable_self_joint must is not None if the address has sent a joint
    let mut self_unit = BUSINESS_CACHE
        .global_state
        .get_last_stable_self_joint(&address);

    while let Some(last_self_unit) = self_unit {
        let self_joint_date = SDAG_CACHE.get_joint(&last_self_unit)?.read()?;

        if address != &self_joint_date.unit.authors[0].address {
            panic!("last self unit first author is not address {}", address);
        }

        // send money to others
        for msg in &self_joint_date.unit.messages {
            if let Some(Payload::Payment(ref payment)) = msg.payload {
                for output in &payment.outputs {
                    // skip ourself change
                    if &output.address == address {
                        continue;
                    }

                    transactions.push(Transaction {
                        unit_hash: last_self_unit.clone(),
                        from_addr: address.clone(),
                        to_addr: output.address.clone(),
                        amount: output.amount,
                        time: self_joint_date.unit.timestamp,
                    });

                    if transactions.len() >= num {
                        return Ok(transactions);
                    }
                }
            }
        }
        // receive money from others
        let related_units = self_joint_date.get_related_units();
        for unit in related_units {
            let related_joint_data = SDAG_CACHE.get_joint(&unit)?.read()?;
            if get_receive_tx(&related_joint_data.unit, address, num, &mut transactions) {
                return Ok(transactions);
            }
        }

        self_unit = self_joint_date.get_stable_prev_self_unit();
    }

    Ok(transactions)
}

/// get Transactions from outputs of unit
/// return true if find all needed tx
fn get_receive_tx(
    unit: &Unit,
    address: &String,
    need_tx_count: usize,
    txs: &mut Vec<Transaction>,
) -> bool {
    for msg in &unit.messages {
        if let Some(Payload::Payment(ref payment)) = msg.payload {
            for output in &payment.outputs {
                if &output.address == address {
                    txs.push(Transaction {
                        unit_hash: unit.unit.clone(),
                        from_addr: unit.authors[0].address.clone(), // just support one author currently
                        to_addr: address.clone(),
                        amount: output.amount,
                        time: unit.timestamp,
                    });

                    if txs.len() >= need_tx_count {
                        return true;
                    }
                }
            }
        }
    }

    false
}
