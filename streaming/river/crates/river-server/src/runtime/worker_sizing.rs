/// A CUDA stream orders submissions, but a parsed state must finish before reuse.
/// All states in a completed wave may be reused without another barrier.
pub fn state_reuse_barrier(index:usize,states:usize)->bool {
    index > 0 && index % states == 0
}

/// One admitted decode leases one output workspace, including its tile union.
pub fn workspaces(workers:usize,decode_slots:usize)->usize {
    workers.min(decode_slots).clamp(1,32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_state_is_exclusive_until_its_wave_finishes() {
        for states in [1,2,8,16] {
            let mut in_flight=std::collections::HashSet::new();
            let mut barriers=Vec::new();
            for index in 0..65 {
                if state_reuse_barrier(index,states) {in_flight.clear();barriers.push(index);}
                assert!(in_flight.insert(index % states),"state reused while in flight");
            }
            assert_eq!(barriers,(states..65).step_by(states).collect::<Vec<_>>());
        }
        assert!(!state_reuse_barrier(0,8));
        assert!(state_reuse_barrier(8,8));
        assert!(!state_reuse_barrier(9,8));
    }
    #[test]
    fn buffers_track_admission_including_small_instances() {
        for slots in [1,2,5,8,16,32] {assert_eq!(workspaces(32,slots),slots);}
    }
    #[test]
    fn fewer_workers_do_not_reserve_unreachable_buffers() {
        assert_eq!(workspaces(4,8),4);
        assert_eq!(workspaces(32,2),2);
    }
    #[test]
    fn pool_remains_within_native_resource_limits() {
        assert_eq!(workspaces(32,0),1);
        assert_eq!(workspaces(1024,1024),32);
        assert_eq!(workspaces(0,8),1);
        assert_eq!(workspaces(1024,8),8);
    }
}
