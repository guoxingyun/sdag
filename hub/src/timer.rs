use std::time::Duration;

use may::coroutine;
use sdag::network::hub;
use sdag::statistics;

pub fn start_global_timers() {
    // request needed joints that were not received during the previous session
    go!(move || loop {
        info!("re_request_lost_joints");
        t!(hub::re_request_lost_joints());
        coroutine::sleep(Duration::from_secs(8));
    });

    // remove those junk joints
    go!(move || loop {
        const TIMEOUT: u64 = 2 * 60; // 2min
        coroutine::sleep(Duration::from_secs(TIMEOUT / 2));
        info!("purge_junk_unhandled_joints");
        hub::purge_junk_unhandled_joints(TIMEOUT * 1000);
    });

    // remove those temp-bad free joints
    go!(move || loop {
        const TIMEOUT: u64 = 60; // 1min
        coroutine::sleep(Duration::from_secs(TIMEOUT / 2));
        info!("purge_tempbad_joints");
        t!(hub::purge_temp_bad_free_joints(TIMEOUT * 1000));
    });

    // auto connection if peers count is under threshold
    go!(move || loop {
        coroutine::sleep(Duration::from_secs(30));
        info!("auto connect to other peers");
        hub::auto_connection();
    });

    // broadcast good free joints list timely
    go!(move || loop {
        coroutine::sleep(Duration::from_secs(10));
        info!("broadcast_free_joint_list to peers");
        hub::broadcast_free_joint_list();
    });

    // reset peer statistics
    go!(move || loop {
        statistics::update_stats();
        coroutine::sleep(Duration::from_secs(1));
    });
}
