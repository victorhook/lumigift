//! `Link::assign_slots_in_order`: slots follow a given (room) order.

use std::time::Duration;

use lumigift_core::sim::SimConfig;
use lumigift_core::*;

#[test]
fn slots_follow_the_given_order_and_others_move_aside() {
    let mut sc = SimConfig::new(5);
    sc.loss_pct = 0.0;
    // Scrambled slots; gift 4 holds slot 1, which the order will need.
    for (g, s) in sc.gifts.iter_mut().zip([3u8, 0, 2, 7, 1]) {
        g.slot = s;
    }
    let cfg = LinkConfig {
        metadata: MetadataLocation::Disabled,
        wake_on_connect: false,
        ..Default::default()
    };
    let (link, _sim) = Link::simulated(cfg, sc);
    let snap = link
        .wait_for(Duration::from_secs(10), |s| {
            s.bridge.state == BridgeState::Connected && s.gifts.iter().filter(|g| g.online).count() == 5
        })
        .expect("all gifts online");
    let mut uids: Vec<(Option<u8>, Uid)> = snap.gifts.iter().map(|g| (g.slot, g.uid)).collect();
    uids.sort();
    // Room order: the gifts that had slots 7, 3, 0, 2 (left to right).
    let by_slot = |s: u8| uids.iter().find(|g| g.0 == Some(s)).unwrap().1;
    let order = vec![by_slot(7), by_slot(3), by_slot(0), by_slot(2)];
    let other = by_slot(1);
    let r = link.assign_slots_in_order(order.clone()).wait().expect("assigned");
    assert!(r.failed.is_empty(), "{r:?}");
    let snap = link.snapshot();
    let slot_of = |u: Uid| snap.gifts.iter().find(|g| g.uid == u).unwrap().slot;
    for (i, u) in order.iter().enumerate() {
        assert_eq!(slot_of(*u), Some(i as u8));
    }
    // The gift that held slot 1 moved to the first free slot.
    assert_eq!(slot_of(other), Some(4));
    link.shutdown();
}
