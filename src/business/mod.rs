use std::collections::HashMap;

use cache::{CachedJoint, JointData, SDAG_CACHE};
use config;
use error::Result;
use joint::{JointSequence, Level};
use may::coroutine::JoinHandle;
use may::sync::{mpsc, RwLock};
use object_hash;
use spec::*;

mod data_feed;
mod text;
mod utxo;

lazy_static! {
    pub static ref BUSINESS_WORKER: BusinessWorker = BusinessWorker::default();
    pub static ref BUSINESS_CACHE: BusinessCache =
        BusinessCache::rebuild_from_db().expect("failed to rebuild business state");
}

//---------------------------------------------------------------------------------------
// Business Trait (for different sub business)
//---------------------------------------------------------------------------------------
// TODO: use this trait in dynamic business registration
pub trait SubBusiness {
    /// validate business basics like format before put joint into cache
    fn validate_message_basic(message: &Message) -> Result<()>;
    /// check sub business before normalize
    fn check_business(joint: &JointData, message_idx: usize) -> Result<()>;
    /// validate if the message/action is valid in current state after joint got stable
    fn validate_message(&self, joint: &JointData, message_idx: usize) -> Result<()>;
    /// apply the message/action to the current business state
    /// this is a specific business state transition
    /// return an error means that something should never happen since we validate first
    /// and you should make sure that the state is rolled back before return error
    fn apply_message(&mut self, joint: &JointData, message_idx: usize) -> Result<()>;
    /// revert temp change if stable validation failed, only for temp state
    fn revert_message(&mut self, joint: &JointData, message_idx: usize) -> Result<()>;
}

//---------------------------------------------------------------------------------------
// BusinessWorker
//---------------------------------------------------------------------------------------
pub struct BusinessWorker {
    tx: mpsc::Sender<CachedJoint>,
    _handler: JoinHandle<()>,
}

impl Default for BusinessWorker {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();

        let _handler = start_business_worker(rx);

        BusinessWorker { tx, _handler }
    }
}

impl BusinessWorker {
    // the main chain logic would call this API to push stable joint in order
    pub fn push_stable_joint(&self, joint: CachedJoint) -> Result<()> {
        self.tx.send(joint)?;
        Ok(())
    }
}

// this would start the global thread to process the stable joints
fn start_business_worker(rx: mpsc::Receiver<CachedJoint>) -> JoinHandle<()> {
    go!(move || {
        while let Ok(joint) = rx.recv() {
            let joint_data = t_c!(joint.read());

            // TODO: spend the commissions first
            // if not enough we should set a special state and skip business validate and apply
            // and the final_stage would clear the content

            // TODO: add state transfer table

            match BUSINESS_CACHE.validate_stable_joint(&joint_data) {
                Ok(_) => {
                    match joint_data.get_sequence() {
                        JointSequence::NonserialBad | JointSequence::TempBad => {
                            // apply the message to temp business state
                            let mut temp_business_state =
                                BUSINESS_CACHE.temp_business_state.write().unwrap();
                            for i in 0..joint_data.unit.messages.len() {
                                if let Err(e) = temp_business_state.apply_message(&joint_data, i) {
                                    error!("apply temp state failed, err = {:?}", e);
                                }
                            }
                        }
                        _ => {}
                    }

                    if let Err(e) = BUSINESS_CACHE.apply_stable_joint(&joint_data) {
                        // apply joint failed which should never happen
                        // but we have to save it as a bad joint
                        // we hope that the global state is still correct
                        // like transactions
                        error!("apply_joint failed, err = {:?}", e);

                        joint_data.set_sequence(JointSequence::FinalBad);
                    }
                }
                Err(e) => {
                    error!("validate_joint failed, err = {:?}", e);
                    match joint_data.get_sequence() {
                        JointSequence::Good => {
                            let mut temp_business_state =
                                BUSINESS_CACHE.temp_business_state.write().unwrap();
                            for i in 0..joint_data.unit.messages.len() {
                                if let Err(e) = temp_business_state.revert_message(&joint_data, i) {
                                    error!("revert temp state failed, err = {:?}", e);
                                }
                            }
                        }
                        _ => {}
                    }

                    joint_data.set_sequence(JointSequence::FinalBad);
                }
            }
            // need to generate ball

            t_c!(::finalization::FINALIZATION_WORKER.push_final_joint(joint));
        }
        error!("business worker stopped!");
        ::std::process::abort();
    })
}

//---------------------------------------------------------------------------------------
// GlobalState
//---------------------------------------------------------------------------------------
#[derive(Default)]
pub struct GlobalState {
    // FIXME: this read lock is some what too heavy, we are only care about one address
    // record author own last stable self joint that he last send
    // HashMap<Address, UnitHash>
    last_stable_self_joint: RwLock<HashMap<String, String>>,
}

impl GlobalState {
    fn get_last_stable_self_joint(&self, address: &str) -> Option<CachedJoint> {
        self.last_stable_self_joint
            .read()
            .unwrap()
            .get(address)
            .and_then(|unit| SDAG_CACHE.get_joint(unit).ok())
    }

    fn update_last_stable_self_joint(&self, joint: &JointData) {
        let unit_hash = joint.get_unit_hash();
        for author in &joint.unit.authors {
            self.last_stable_self_joint
                .write()
                .unwrap()
                .entry(author.address.clone())
                .and_modify(|v| *v = unit_hash.to_owned())
                .or_insert(unit_hash.clone());
        }
    }

    /// rebuild from database
    /// TODO: rebuild from database
    /// NOTE: need also update global state and temp business state
    pub fn rebuild_from_db() -> Result<Self> {
        Ok(GlobalState::default())
    }
}

//---------------------------------------------------------------------------------------
// BusinessState
//---------------------------------------------------------------------------------------
#[derive(Default)]
pub struct BusinessState {
    // below is sub business
    utxo: utxo::UtxoCache,
    text: text::TextCache,
    data_feed: data_feed::TimerCache,
    // TODO: dynamic business (use Anymap?)
}

impl BusinessState {
    fn validate_message_basic(message: &Message) -> Result<()> {
        // each sub business format check
        match message.app.as_str() {
            "payment" => utxo::UtxoCache::validate_message_basic(message)?,
            "text" => text::TextCache::validate_message_basic(message)?,
            "data_feed" => data_feed::TimerCache::validate_message_basic(message)?,
            _ => bail!("unsupported business"),
        }
        Ok(())
    }

    fn check_business(joint: &JointData, message_idx: usize) -> Result<()> {
        let message = &joint.unit.messages[message_idx];
        match message.app.as_str() {
            "payment" => utxo::UtxoCache::check_business(joint, message_idx)?,
            "text" => text::TextCache::check_business(joint, message_idx)?,
            "data_feed" => data_feed::TimerCache::check_business(joint, message_idx)?,
            _ => bail!("unsupported business"),
        }
        Ok(())
    }

    fn validate_message(&self, joint: &JointData, message_idx: usize) -> Result<()> {
        let message = &joint.unit.messages[message_idx];
        match message.app.as_str() {
            "payment" => self.utxo.validate_message(joint, message_idx)?,
            "text" => self.text.validate_message(joint, message_idx)?,
            "data_feed" => self.data_feed.validate_message(joint, message_idx)?,
            _ => bail!("unsupported business"),
        }
        Ok(())
    }

    fn apply_message(&mut self, joint: &JointData, message_idx: usize) -> Result<()> {
        let message = &joint.unit.messages[message_idx];
        match message.app.as_str() {
            "payment" => self.utxo.apply_message(joint, message_idx)?,
            "text" => self.text.apply_message(joint, message_idx)?,
            "data_feed" => self.data_feed.apply_message(joint, message_idx)?,
            _ => bail!("unsupported business"),
        }
        Ok(())
    }

    // only temp state would call this api
    fn revert_message(&mut self, joint: &JointData, message_idx: usize) -> Result<()> {
        let message = &joint.unit.messages[message_idx];
        match message.app.as_str() {
            "payment" => self.utxo.revert_message(joint, message_idx)?,
            "text" => self.text.revert_message(joint, message_idx)?,
            "data_feed" => self.data_feed.revert_message(joint, message_idx)?,
            _ => bail!("unsupported business"),
        }
        Ok(())
    }
}

//---------------------------------------------------------------------------------------
// BusinessCache
//---------------------------------------------------------------------------------------
#[derive(Default)]
pub struct BusinessCache {
    // TODO: lock global is not necessary for each address
    global_state: GlobalState,
    business_state: RwLock<BusinessState>,
    temp_business_state: RwLock<BusinessState>,
}

impl BusinessCache {
    /// build the state from genesis
    /// TODO: also need to rebuild temp state (same as state)
    pub fn rebuild_from_genesis() -> Result<Self> {
        let business_cache = BusinessCache::default();
        let mut mci = Level::new(0);

        while let Ok(next_joints) = SDAG_CACHE.get_joints_by_mci(mci) {
            if next_joints.is_empty() {
                break;
            }

            for joint in next_joints.into_iter() {
                let joint = joint.read()?;

                if joint.get_sequence() == JointSequence::Good {
                    business_cache.apply_stable_joint(&joint)?;
                }
            }
            mci += 1;
        }

        Ok(business_cache)
    }

    /// rebuild from database
    /// TODO: rebuild from database
    /// NOTE: need also update global state and temp business state
    pub fn rebuild_from_db() -> Result<Self> {
        Ok(BusinessCache::default())
    }

    /// validate if contains last stable self unit
    pub fn is_include_last_stable_self_joint(&self, joint: &JointData) -> Result<()> {
        for author in &joint.unit.authors {
            match self
                .global_state
                .get_last_stable_self_joint(&author.address)
            {
                None => continue,
                Some(author_joint) => {
                    // joint is not include author joint
                    if !(joint > &*author_joint.read()?) {
                        bail!("joint not include last stable self unit");
                    }
                }
            };
        }

        Ok(())
    }

    /// validate unstable joint with no global order
    pub fn validate_unstable_joint(&self, joint: &JointData) -> Result<JointSequence> {
        // global check
        let state = validate_unstable_joint_serial(joint)?;
        if state != JointSequence::Good {
            return Ok(state);
        }

        // for each message do business related validation
        for i in 0..joint.unit.messages.len() {
            let state = self
                .temp_business_state
                .read()
                .unwrap()
                .validate_message(joint, i);
            if let Err(e) = state {
                error!("validate_unstable_joint, err={}", e);
                return Ok(JointSequence::TempBad);
            } else {
                // unordered validate pass, apply it
                self.temp_business_state
                    .write()
                    .unwrap()
                    .apply_message(joint, i)?;
            }
        }

        Ok(JointSequence::Good)
    }

    /// validate stable joint with global order
    fn validate_stable_joint(&self, joint: &JointData) -> Result<()> {
        // TODO: check if enough commission here
        // for each message do business related validation
        if joint.get_sequence() == JointSequence::FinalBad {
            bail!("joint is already set to finalbad, unit={}", joint.unit.unit);
        }

        self.is_include_last_stable_self_joint(joint)?;

        let business_state = self.business_state.read().unwrap();
        for i in 0..joint.unit.messages.len() {
            business_state.validate_message(joint, i)?;
        }

        Ok(())
    }

    /// apply changes, save the new state
    fn apply_stable_joint(&self, joint: &JointData) -> Result<()> {
        // TODO: deduce the commission

        // update global state
        self.global_state.update_last_stable_self_joint(joint);

        let mut business_state = self.business_state.write().unwrap();

        for i in 0..joint.unit.messages.len() {
            business_state.apply_message(joint, i)?;
        }

        Ok(())
    }
}

//---------------------------------------------------------------------------------------
// Global functions
//---------------------------------------------------------------------------------------
pub fn validate_business_basic(unit: &Unit) -> Result<()> {
    validate_headers_commission_recipients(unit)?;

    for i in 0..unit.messages.len() {
        let message = &unit.messages[i];
        validate_message_format(message)?;
        validate_message_payload(message)?;
        BusinessState::validate_message_basic(message)?;
    }

    Ok(())
}

pub fn check_business(joint: &JointData) -> Result<()> {
    // for each message do business related validation
    for i in 0..joint.unit.messages.len() {
        BusinessState::check_business(joint, i)?;
    }
    Ok(())
}

// 1) if has multi authors , unit.earned_headers_commission_recipients must not be empty;
// 2) address of unit.earned_headers_commission_recipients should ordered by address
// 3) total earned_headers_commission_share of unit.earned_headers_commission_recipients must be 100
fn validate_headers_commission_recipients(unit: &Unit) -> Result<()> {
    if unit.authors.len() > 1 && unit.earned_headers_commission_recipients.is_empty() {
        bail!("must specify earned_headers_commission_recipients when more than 1 author");
    }

    if unit.earned_headers_commission_recipients.is_empty() {
        return Ok(());
    }

    let mut total_earned_headers_commission_share = 0;
    let mut prev_address = "".to_owned();
    for recipient in &unit.earned_headers_commission_recipients {
        if recipient.address <= prev_address {
            bail!("recipient list must be sorted by address");
        }
        if !object_hash::is_chash_valid(&recipient.address) {
            bail!("invalid recipient address checksum");
        }
        total_earned_headers_commission_share += recipient.earned_headers_commission_share;
        prev_address = recipient.address.clone();
    }

    if total_earned_headers_commission_share != 100 {
        bail!("sum of earned_headers_commission_share is not 100");
    }

    Ok(())
}

fn validate_message_payload(message: &Message) -> Result<()> {
    if message.payload_hash.len() != config::HASH_LENGTH {
        bail!("wrong payload hash size");
    }

    if message.payload.is_none() {
        bail!("no inline payload");
    }

    let payload_hash = object_hash::get_base64_hash(message.payload.as_ref().unwrap())?;
    if payload_hash != message.payload_hash {
        bail!(
            "wrong payload hash: expected {}, got {}",
            payload_hash,
            message.payload_hash
        );
    }

    Ok(())
}

fn validate_message_format(msg: &Message) -> Result<()> {
    if msg.payload_location != "inline"
        && msg.payload_location != "uri"
        && msg.payload_location != "none"
    {
        bail!("wrong payload location: {}", msg.payload_location);
    }

    if msg.payload_location != "uri" {
        if msg.payload_uri.is_some() && msg.payload_uri_hash.is_some() {
            bail!("must not contain payload_uri and payload_uri_hash");
        }
    }

    Ok(())
}

fn validate_unstable_joint_serial(joint: &JointData) -> Result<JointSequence> {
    // check unstable joints non serial
    let cached_joint = SDAG_CACHE.try_get_joint(joint.get_unit_hash()).unwrap();

    if crate::serial_check::is_unstable_joint_non_serial(cached_joint)? {
        return Ok(JointSequence::NonserialBad);
    }

    Ok(JointSequence::Good)
}
