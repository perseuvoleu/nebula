//! Memory admission for heavy commands and agent sessions: one FIFO queue
//! per machine, admitting its head only while the configured budget and the
//! machine's live memory both have room. Pure: callers feed it memory
//! readings and RSS samples and act on the consumers it admits.

use nebula_core::mem::{MachineMemory, ProbeError, VmPressureLevel};
use nebula_core::AgentId;
use std::collections::VecDeque;

/// A reservation whose size is unknown: nothing is admitted beside it.
pub const UNBOUNDED_BYTES: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub budget_bytes: u64,
    pub floor_bytes: u64,
    pub psi_some_avg10_max: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeaseId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Consumer {
    Command(LeaseId),
    Session(AgentId),
}

use nebula_core::Blocked;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Queued(usize),
    Running,
}

#[derive(Debug, Default, PartialEq)]
pub struct Pump {
    pub admitted: Vec<Consumer>,
    pub head_blocked: Option<Blocked>,
}

#[derive(Debug)]
struct Running {
    consumer: Consumer,
    reserved_bytes: u64,
    rss_bytes: u64,
}

impl Running {
    fn held_bytes(&self) -> u64 {
        self.reserved_bytes.max(self.rss_bytes)
    }

    fn reserved_unseen_in_rss_bytes(&self) -> u64 {
        self.reserved_bytes.saturating_sub(self.rss_bytes)
    }
}

#[derive(Debug)]
pub struct Dispatcher {
    limits: Limits,
    running: Vec<Running>,
    queue: VecDeque<(Consumer, u64)>,
}

impl Dispatcher {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            running: Vec::new(),
            queue: VecDeque::new(),
        }
    }

    /// A consumer already queued or running keeps its place and its first reservation.
    pub fn enqueue(&mut self, consumer: Consumer, reserved_bytes: u64) -> Standing {
        if let Some(standing) = self.standing(&consumer) {
            return standing;
        }
        self.queue.push_back((consumer, reserved_bytes));
        Standing::Queued(self.queue.len() - 1)
    }

    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    pub fn has_queued(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Count a consumer that is already running, skipping the queue.
    pub fn admit_running(&mut self, consumer: Consumer, reserved_bytes: u64) {
        if self.standing(&consumer) == Some(Standing::Running) {
            return;
        }
        self.queue.retain(|(c, _)| c != &consumer);
        self.running.push(Running {
            consumer,
            reserved_bytes,
            rss_bytes: 0,
        });
    }

    /// Drop a consumer, running or still queued.
    pub fn release(&mut self, consumer: &Consumer) {
        self.running.retain(|r| &r.consumer != consumer);
        self.queue.retain(|(c, _)| c != consumer);
    }

    pub fn observe_rss(&mut self, consumer: &Consumer, rss_bytes: u64) {
        if let Some(r) = self.running.iter_mut().find(|r| &r.consumer == consumer) {
            r.rss_bytes = rss_bytes;
        }
    }

    /// Move a running consumer to a new key, keeping its measured RSS.
    pub fn rekey_running(&mut self, from: &Consumer, to: Consumer, reserved_bytes: u64) {
        let rss_bytes = self
            .running
            .iter()
            .find(|r| &r.consumer == from)
            .map_or(0, |r| r.rss_bytes);
        self.release(from);
        self.admit_running(to.clone(), reserved_bytes);
        self.observe_rss(&to, rss_bytes);
    }

    /// Queue positions are zero-based.
    pub fn standing(&self, consumer: &Consumer) -> Option<Standing> {
        if self.running.iter().any(|r| &r.consumer == consumer) {
            return Some(Standing::Running);
        }
        self.queue
            .iter()
            .position(|(c, _)| c == consumer)
            .map(Standing::Queued)
    }

    /// Admit queue heads in order until one does not fit.
    pub fn pump(&mut self, machine: &Result<MachineMemory, ProbeError>) -> Pump {
        let mut pump = Pump::default();
        while let Some((consumer, reserved_bytes)) = self.queue.front().cloned() {
            if let Err(blocked) = self.room_for(reserved_bytes, machine) {
                pump.head_blocked = Some(blocked);
                break;
            }
            self.queue.pop_front();
            self.running.push(Running {
                consumer: consumer.clone(),
                reserved_bytes,
                rss_bytes: 0,
            });
            pump.admitted.push(consumer);
        }
        pump
    }

    fn room_for(
        &self,
        reserved_bytes: u64,
        machine: &Result<MachineMemory, ProbeError>,
    ) -> Result<(), Blocked> {
        let held_after = self
            .running
            .iter()
            .map(Running::held_bytes)
            .try_fold(reserved_bytes, u64::checked_add);
        if self
            .running
            .iter()
            .any(|r| r.reserved_bytes == UNBOUNDED_BYTES)
            || held_after.is_none_or(|bytes| bytes > self.limits.budget_bytes)
        {
            return Err(Blocked::OverBudget);
        }
        let unseen_reservations = self
            .running
            .iter()
            .map(Running::reserved_unseen_in_rss_bytes)
            .fold(0, u64::saturating_add);
        match machine {
            Err(_) => Err(Blocked::ProbeFailed),
            Ok(MachineMemory::Linux {
                available_bytes,
                psi_some_avg10,
                ..
            }) => {
                if *psi_some_avg10 > self.limits.psi_some_avg10_max {
                    Err(Blocked::Pressure)
                } else if self
                    .limits
                    .floor_bytes
                    .checked_add(reserved_bytes)
                    .is_none_or(|needed| {
                        available_bytes.saturating_sub(unseen_reservations) < needed
                    })
                {
                    Err(Blocked::LowMemory)
                } else {
                    Ok(())
                }
            }
            Ok(MachineMemory::Macos {
                swap_free_bytes,
                pressure,
                ..
            }) => {
                if *pressure != VmPressureLevel::Normal {
                    Err(Blocked::Pressure)
                } else if *swap_free_bytes < self.limits.floor_bytes {
                    Err(Blocked::LowMemory)
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;
    const LIMITS: Limits = Limits {
        budget_bytes: 7 * GIB,
        floor_bytes: 2 * GIB,
        psi_some_avg10_max: 10.0,
    };

    fn linux(available_gib: u64, psi: f64) -> Result<MachineMemory, ProbeError> {
        Ok(MachineMemory::Linux {
            available_bytes: available_gib * GIB,
            swap_free_bytes: 0,
            psi_some_avg10: psi,
        })
    }

    fn macos(swap_free_gib: u64, pressure: VmPressureLevel) -> Result<MachineMemory, ProbeError> {
        Ok(MachineMemory::Macos {
            total_bytes: 32 * GIB,
            swap_free_bytes: swap_free_gib * GIB,
            pressure,
        })
    }

    fn cmd(n: u64) -> Consumer {
        Consumer::Command(LeaseId(n))
    }

    fn session(id: &str) -> Consumer {
        Consumer::Session(AgentId(id.into()))
    }

    struct Job {
        reserved: u64,
        rss: u64,
    }

    fn running(jobs: &[Job]) -> Dispatcher {
        let mut d = Dispatcher::new(LIMITS);
        for (i, &Job { reserved, rss }) in jobs.iter().enumerate() {
            d.enqueue(cmd(100 + i as u64), reserved);
            assert_eq!(
                d.pump(&linux(64, 0.0)).admitted.len(),
                1,
                "setup admits job {i}"
            );
            d.observe_rss(&cmd(100 + i as u64), rss);
        }
        d
    }

    struct Case {
        name: &'static str,
        running: &'static [Job],
        request: u64,
        machine: Result<MachineMemory, ProbeError>,
        blocked: Option<Blocked>,
    }

    #[test]
    fn admission_needs_budget_headroom_and_live_room() {
        use Blocked::*;
        use VmPressureLevel::*;
        let ample = || linux(64, 0.0);
        let cases: [Case; 17] = [
            Case {
                name: "empty machine admits",
                running: &[],
                request: 4 * GIB,
                machine: ample(),
                blocked: None,
            },
            Case {
                name: "request filling the budget exactly admits",
                running: &[Job {
                    reserved: 3 * GIB,
                    rss: 0,
                }],
                request: 4 * GIB,
                machine: ample(),
                blocked: None,
            },
            Case {
                name: "one byte over the budget waits",
                running: &[Job {
                    reserved: 3 * GIB,
                    rss: 0,
                }],
                request: 4 * GIB + 1,
                machine: ample(),
                blocked: Some(OverBudget),
            },
            Case {
                name: "rss above its reservation counts",
                running: &[Job {
                    reserved: GIB,
                    rss: 4 * GIB,
                }],
                request: 4 * GIB,
                machine: ample(),
                blocked: Some(OverBudget),
            },
            Case {
                name: "rss below its reservation counts the reservation",
                running: &[Job {
                    reserved: 4 * GIB,
                    rss: GIB,
                }],
                request: 4 * GIB,
                machine: ample(),
                blocked: Some(OverBudget),
            },
            Case {
                name: "rss at its reservation is not counted twice",
                running: &[Job {
                    reserved: 3 * GIB,
                    rss: 3 * GIB,
                }],
                request: 4 * GIB,
                machine: ample(),
                blocked: None,
            },
            Case {
                name: "available exactly floor plus request admits",
                running: &[],
                request: 4 * GIB,
                machine: linux(6, 0.0),
                blocked: None,
            },
            Case {
                name: "available below floor plus request waits",
                running: &[],
                request: 4 * GIB,
                machine: linux(5, 0.0),
                blocked: Some(LowMemory),
            },
            Case {
                name: "pending reservation comes off available",
                running: &[Job {
                    reserved: 2 * GIB,
                    rss: 0,
                }],
                request: GIB,
                machine: linux(4, 0.0),
                blocked: Some(LowMemory),
            },
            Case {
                name: "only the unseen part of a reservation is pending",
                running: &[Job {
                    reserved: 2 * GIB,
                    rss: GIB,
                }],
                request: GIB,
                machine: linux(4, 0.0),
                blocked: None,
            },
            Case {
                name: "psi at the threshold admits",
                running: &[],
                request: GIB,
                machine: linux(64, 10.0),
                blocked: None,
            },
            Case {
                name: "psi above the threshold waits",
                running: &[],
                request: GIB,
                machine: linux(64, 10.5),
                blocked: Some(Pressure),
            },
            Case {
                name: "macos normal with free swap admits",
                running: &[],
                request: 4 * GIB,
                machine: macos(2, Normal),
                blocked: None,
            },
            Case {
                name: "macos warn level waits",
                running: &[],
                request: GIB,
                machine: macos(8, Warn),
                blocked: Some(Pressure),
            },
            Case {
                name: "macos critical level waits",
                running: &[],
                request: GIB,
                machine: macos(8, Critical),
                blocked: Some(Pressure),
            },
            Case {
                name: "macos swap below the floor waits",
                running: &[],
                request: GIB,
                machine: macos(1, Normal),
                blocked: Some(LowMemory),
            },
            Case {
                name: "failed probe waits",
                running: &[],
                request: GIB,
                machine: Err(ProbeError::Unreadable("x".into())),
                blocked: Some(ProbeFailed),
            },
        ];
        for Case {
            name,
            running: jobs,
            request,
            machine,
            blocked,
        } in cases
        {
            let mut d = running(jobs);
            d.enqueue(cmd(1), request);
            let expected = Pump {
                admitted: if blocked.is_none() {
                    vec![cmd(1)]
                } else {
                    vec![]
                },
                head_blocked: blocked,
            };
            assert_eq!(d.pump(&machine), expected, "{name}");
        }
    }

    #[test]
    fn mixed_queue_is_strict_fifo() {
        let ample = linux(64, 0.0);
        let mut d = running(&[Job {
            reserved: 5 * GIB,
            rss: 0,
        }]);
        d.enqueue(session("a"), 3 * GIB);
        d.enqueue(cmd(2), GIB);
        d.enqueue(session("b"), GIB);
        assert_eq!(
            d.pump(&ample),
            Pump {
                admitted: vec![],
                head_blocked: Some(Blocked::OverBudget)
            },
            "a smaller request behind a blocked head still waits"
        );
        assert_eq!(d.standing(&cmd(2)), Some(Standing::Queued(1)));

        d.release(&cmd(100));
        assert_eq!(
            d.pump(&ample),
            Pump {
                admitted: vec![session("a"), cmd(2), session("b")],
                head_blocked: None
            },
            "freed room admits heads in order while they fit"
        );
        assert_eq!(d.standing(&session("b")), Some(Standing::Running));
    }

    #[test]
    fn release_admits_only_the_next_fitting_head() {
        let ample = linux(64, 0.0);
        let mut d = running(&[
            Job {
                reserved: 4 * GIB,
                rss: 0,
            },
            Job {
                reserved: 3 * GIB,
                rss: 0,
            },
        ]);
        d.enqueue(cmd(1), 3 * GIB);
        d.enqueue(session("a"), 3 * GIB);
        d.enqueue(cmd(2), GIB);
        assert_eq!(d.pump(&ample).admitted, vec![]);

        d.release(&cmd(101));
        assert_eq!(
            d.pump(&ample),
            Pump {
                admitted: vec![cmd(1)],
                head_blocked: Some(Blocked::OverBudget)
            },
            "the next head fits, the one after does not, and the 1 GiB job behind it may not jump"
        );
        assert_eq!(d.standing(&session("a")), Some(Standing::Queued(0)));
        assert_eq!(d.standing(&cmd(2)), Some(Standing::Queued(1)));
    }

    #[test]
    fn enqueueing_a_queued_or_running_consumer_again_holds_one_place() {
        let ample = linux(64, 0.0);
        let mut d = Dispatcher::new(LIMITS);
        d.enqueue(session("a"), GIB);
        assert_eq!(d.enqueue(session("a"), GIB), Standing::Queued(0));
        d.enqueue(session("b"), 6 * GIB);
        assert_eq!(
            d.pump(&ample),
            Pump {
                admitted: vec![session("a"), session("b")],
                head_blocked: None
            },
            "a repeated enqueue is counted once"
        );
        assert_eq!(d.enqueue(session("a"), GIB), Standing::Running);
        assert_eq!(
            d.pump(&ample),
            Pump::default(),
            "a running consumer is not queued again"
        );
    }

    #[test]
    fn reservations_too_large_to_add_up_wait() {
        let mut d = running(&[Job {
            reserved: GIB,
            rss: 0,
        }]);
        d.enqueue(cmd(1), u64::MAX);
        assert_eq!(
            d.pump(&linux(64, 0.0)),
            Pump {
                admitted: vec![],
                head_blocked: Some(Blocked::OverBudget)
            },
            "held plus request"
        );

        let mut d = Dispatcher::new(Limits {
            budget_bytes: u64::MAX,
            ..LIMITS
        });
        d.enqueue(cmd(1), u64::MAX);
        assert_eq!(
            d.pump(&linux(64, 0.0)),
            Pump {
                admitted: vec![],
                head_blocked: Some(Blocked::LowMemory)
            },
            "floor plus request"
        );
    }

    #[test]
    fn an_unbounded_reservation_admits_nothing_beside_it() {
        let mut d = Dispatcher::new(Limits {
            budget_bytes: u64::MAX,
            floor_bytes: 0,
            ..LIMITS
        });
        d.admit_running(cmd(100), UNBOUNDED_BYTES);
        d.observe_rss(&cmd(100), GIB);
        d.enqueue(cmd(1), 0);
        assert_eq!(
            d.pump(&linux(64, 0.0)),
            Pump {
                admitted: vec![],
                head_blocked: Some(Blocked::OverBudget)
            },
            "a zero-byte request under an unbounded budget"
        );
    }

    #[test]
    fn releasing_a_queued_consumer_unblocks_the_one_behind() {
        let mut d = running(&[Job {
            reserved: 4 * GIB,
            rss: 0,
        }]);
        d.enqueue(session("big"), 6 * GIB);
        d.enqueue(cmd(1), 2 * GIB);
        assert_eq!(d.pump(&linux(64, 0.0)).admitted, vec![]);
        d.release(&session("big"));
        assert_eq!(d.pump(&linux(64, 0.0)).admitted, vec![cmd(1)]);
    }
}
