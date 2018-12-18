extern crate sdag_wallet_base;

use std::sync::Arc;
use std::time::{Duration, Instant};

use may::sync::RwLock;
use sdag::business::BUSINESS_CACHE;
use sdag::cache::{CachedJoint, SDAG_CACHE};
use sdag::error::Result;
use sdag::joint::JointSequence;
use sdag::main_chain;
use sdag::my_witness::MY_WITNESSES;
use sdag::utils::AtomicLock;
use sdag_wallet_base::Base64KeyExt;
use serde_json;
use std::collections::{HashSet, VecDeque};
use WALLET_INFO;

lazy_static! {
    static ref IS_WITNESSING: AtomicLock = AtomicLock::new();
    static ref EVENT_TIMER: Arc<RwLock<Option<Instant>>> = Arc::new(RwLock::new(None));
    pub static ref WALLET_PUBK: String = WALLET_INFO._00_address_pubk.to_base64_key();
}

const THRESHOLD_DISTANCE: i32 = 8;

pub fn witness_timer_check() -> Result<Duration> {
    match check_timeout() {
        None => {
            if is_need_witnessing()? {
                witness()?;
            }
            *EVENT_TIMER.write().unwrap() = None;
            Ok(Duration::from_secs(1))
        }
        Some(dur) => Ok(dur),
    }
}

fn set_timeout(sleep_time_ms: u64) {
    let next_expire = Instant::now() + Duration::from_millis(sleep_time_ms);
    let mut g = EVENT_TIMER.write().unwrap();
    if Some(next_expire) > *g {
        *g = Some(next_expire);
    }
}

// when check_timeout return None means we need to take action
// when return Some(duration) means we need to sleep duration for next check
#[inline]
fn check_timeout() -> Option<Duration> {
    let g = EVENT_TIMER.read().unwrap();
    match *g {
        None => Some(Duration::from_secs(1)),
        Some(time) => {
            let now = Instant::now();
            if now >= time {
                None
            } else {
                Some(time - now)
            }
        }
    }
}

pub fn check_and_witness() -> Result<()> {
    info!("check and witness");
    let _g = match IS_WITNESSING.try_lock() {
        Some(g) => g,
        None => {
            info!("witnessing under way");
            return Ok(());
        }
    };

    if is_my_witnessing_unstable(&WALLET_INFO._00_address)? {
        info!("my units is not stable");
        return Ok(());
    }

    adjust_witnessing_speed(&WALLET_INFO._00_address)?;

    Ok(())
}

/// check if unstable joints have my witnessing
fn is_my_witnessing_unstable(my_address: &str) -> Result<bool> {
    let free_joints = SDAG_CACHE.get_free_joints()?;
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();

    for joint in free_joints {
        queue.push_back(joint);
    }

    while let Some(joint) = queue.pop_front() {
        let joint_data = joint.read()?;

        if !visited.insert(joint.key.clone()) || joint_data.is_stable() {
            continue;
        }

        for author in &joint_data.unit.authors {
            if &author.address == my_address {
                return Ok(true);
            }
        }

        for p in joint_data.parents.iter() {
            queue.push_back(p.clone());
        }
    }

    Ok(false)
}

/// adjust witnessing speed: increase speed if (last_mci - my_last_mci) > 8
fn adjust_witnessing_speed(my_address: &str) -> Result<()> {
    let timer;
    let timer_distance;

    let last_mci = main_chain::get_last_stable_mci().value() as i32;

    let my_last_mci = match BUSINESS_CACHE
        .global_state
        .get_last_stable_self_joint(my_address)
    {
        Some(unit) => SDAG_CACHE.get_joint(&unit)?.read()?.get_mci().value() as i32,
        None => -1_000,
    };

    let mci_distance = last_mci - my_last_mci;
    debug!("max_mci is {}, my_max_mci is {}", last_mci, my_last_mci);

    if mci_distance > THRESHOLD_DISTANCE {
        debug!("distance above threshold, will witness");
        timer_distance = THRESHOLD_DISTANCE as f32 / mci_distance as f32;
        // witnessing the first joint, increase the random range
        timer = if last_mci < THRESHOLD_DISTANCE && my_last_mci == -1_000 {
            300
        } else {
            100
        };
    } else {
        timer_distance = (THRESHOLD_DISTANCE - mci_distance) as f32;
        timer = 1_000;
    }

    use rand::{thread_rng, Rng};
    let mut rng = thread_rng();
    let timeout = ((timer_distance + rng.gen_range(0.0, 1.0)) * timer as f32).round() as u64;
    info!(
        "scheduling unconditional witnessing in {} ms unless a new unit arrives.",
        timeout
    );
    set_timeout(timeout);

    Ok(())
}

/// witnessing condition:
/// 1) last self unstable joint is relative stable to free joint, that means the path from free joint to my last unstable joint have more than 6 diff witnesses
/// 2) non witness joint mci > min retrievable mci, min retrievable is last_stable_joint's last_stable_unit mci
/// 3) last self unstable joint support current main chain, that means current main chain include my last unstable joint (cancel)
fn is_need_witnessing() -> Result<(bool)> {
    let _g = match IS_WITNESSING.try_lock() {
        Some(g) => g,
        None => {
            info!("witness_before_threshold under way");
            return Ok(false);
        }
    };

    let free_joints = SDAG_CACHE.get_free_joints()?;

    if free_joints.is_empty() {
        return Ok(false);
    }

    let (need_witness, has_normal_joint) = is_relative_stable(&free_joints)?;

    if !need_witness {
        return Ok(false);
    }

    if has_normal_joint {
        return Ok(true);
    }

    is_normal_joint_behind_min_retrievable(&free_joints)
}

/// return true if more than 6 different other witnesses from best free joints until stable
/// return true if has unstable normal joints
fn is_relative_stable(free_joints: &[CachedJoint]) -> Result<(bool, bool)> {
    let mut best_free_parent = sdag::main_chain::find_best_joint(free_joints.iter())?
        .ok_or_else(|| format_err!("empty best joint amoun free joints"))?
        .read()?;

    let mut has_normal_joints = false;

    let mut diff_witnesses = HashSet::new();
    while !best_free_parent.is_stable() {
        for author in &best_free_parent.unit.authors {
            if WALLET_INFO._00_address == author.address {
                return Ok((false, has_normal_joints));
            }

            if MY_WITNESSES.contains(&author.address) {
                diff_witnesses.insert(author.address.clone());
            } else {
                has_normal_joints = true;
            }
        }

        // need at least half other witnesses
        if diff_witnesses.len() >= sdag::config::MAJORITY_OF_WITNESSES - 1 {
            break;
        }

        best_free_parent = best_free_parent.get_best_parent().read()?;
    }

    Ok((true, has_normal_joints))
}

/// return true if non witness joint behind min retrievable mci, it is very heavy!!!
fn is_normal_joint_behind_min_retrievable(free_joints: &[CachedJoint]) -> Result<bool> {
    let min_retrievable_mci = get_min_retrievable_unit(free_joints)?.read()?.get_mci();

    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();

    for joint in free_joints {
        queue.push_back(joint.clone());
    }

    while let Some(joint) = queue.pop_front() {
        let joint_data = joint.read()?;
        let mci = joint_data.get_mci();

        if !visited.insert(joint.key.clone()) || mci <= min_retrievable_mci {
            continue;
        }

        for author in &joint_data.unit.authors {
            if !MY_WITNESSES.contains(&author.address)
                && joint_data.get_sequence() == JointSequence::Good
            {
                return Ok(true);
            }
        }

        for p in joint_data.parents.iter() {
            queue.push_back(p.clone());
        }
    }

    Ok(false)
}

/// get min retrievable unit: last stable unit's last stable unit
fn get_min_retrievable_unit(free_joints: &[CachedJoint]) -> Result<CachedJoint> {
    // we can unwrap here because free joints is not empty
    let best_joint = sdag::main_chain::find_best_joint(free_joints.iter())?.unwrap();

    match best_joint.read()?.unit.last_ball_unit {
        Some(ref unit) => SDAG_CACHE.get_joint(unit),
        None => Ok(best_joint), // only genesis has no last ball unit
    }
}

#[derive(Serialize)]
struct TimeStamp {
    timestamp: u64,
}

/// compose witness joint and validate, save, broadcast
fn witness() -> Result<()> {
    // divide one output into two outputs, to increase witnessing concurrent performance
    // let amount = divide_money(&WALLET_INFO._00_address)?;

    let sdag::composer::ParentsAndLastBall {
        parents,
        last_ball,
        last_ball_unit,
    } = sdag::composer::pick_parents_and_last_ball(&WALLET_INFO._00_address)?;

    // at most we need another 1000 sdg (usually 431 + 197)
    let (inputs, amount) =
        BUSINESS_CACHE.get_inputs_for_amount(&WALLET_INFO._00_address, 1_000 as u64, false)?;

    let light_props = sdag::light::LightProps {
        last_ball: last_ball,
        last_ball_unit: last_ball_unit,
        parent_units: parents,
        witness_list_unit: sdag::config::get_genesis_unit(),
        has_definition: SDAG_CACHE
            .get_definition(&WALLET_INFO._00_address)
            .is_some(),
    };

    let mut compose_info = sdag::composer::ComposeInfo {
        paid_address: WALLET_INFO._00_address.clone(),
        change_address: WALLET_INFO._00_address.clone(),
        outputs: Vec::new(),
        inputs: sdag::light::InputsResponse { inputs, amount },
        transaction_amount: amount,
        text_message: None,
        light_props: light_props,
        pubk: WALLET_PUBK.clone(),
    };

    if sdag::config::get_need_post_timestamp() {
        let time_stamp = TimeStamp {
            timestamp: sdag::time::now() / 1_000,
        };
        let data_feed_msg = sdag::spec::Message {
            app: "data_feed".to_string(),
            payload_location: "inline".to_string(),
            payload_hash: sdag::object_hash::get_base64_hash(&time_stamp)?,
            payload: Some(sdag::spec::Payload::Other(serde_json::to_value(
                time_stamp,
            )?)),
            payload_uri: None,
            payload_uri_hash: None,
            spend_proofs: Vec::new(),
        };

        compose_info.text_message = Some(data_feed_msg);
    }

    let joint = sdag::composer::compose_joint(compose_info, &*WALLET_INFO)?;

    let cached_joint = match SDAG_CACHE.add_new_joint(joint.clone()) {
        Ok(j) => j,
        Err(e) => {
            warn!("add_new_joint: {}", e);
            return Ok(());
        }
    };
    let joint_data = cached_joint.read().unwrap();
    if joint_data.unit.content_hash.is_some() {
        joint_data.set_sequence(JointSequence::FinalBad);
    }

    if joint_data.is_missing_parent()
        || sdag::validation::validate_ready_joint(cached_joint).is_err()
    {
        bail!("compose new joint is invalid, joint is [{:?}]", &joint);
    }

    sdag::network::hub::WSS.broadcast_joint(&joint)?;

    Ok(())
}