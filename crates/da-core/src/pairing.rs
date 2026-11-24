//! Pairing: a 6-digit code shown on the responder, typed on the initiator.
//!
//! Both sides run SPAKE2 keyed by the code inside the (not yet trusted) TLS
//! session. The resulting key is confirmed with MACs that cover a TLS
//! exporter value and both certificate fingerprints, so a man in the middle
//! who terminated TLS separately with each side cannot complete pairing even
//! if they also knew the code. An attacker gets exactly one online guess per
//! attempt, and the responder locks pairing after repeated failures.

use crate::identity::Fingerprint;
use anyhow::{anyhow, Result};
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

type HmacSha256 = Hmac<Sha256>;

pub const CODE_TTL: Duration = Duration::from_secs(120);
pub const MAX_FAILURES: usize = 5;
pub const FAILURE_WINDOW: Duration = Duration::from_secs(600);
pub const LOCKOUT: Duration = Duration::from_secs(600);
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-displayanything-pairing-v1";

const ID_INITIATOR: &[u8] = b"displayanything-initiator";
const ID_RESPONDER: &[u8] = b"displayanything-responder";

/// A fresh 6-digit code, zero-padded.
pub fn new_code() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

/// Normalise what a person typed: keep digits only ("123 456" -> "123456").
pub fn normalise_code(input: &str) -> String {
    input.chars().filter(|c| c.is_ascii_digit()).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

pub struct PakeState {
    role: Role,
    inner: Spake2<Ed25519Group>,
}

impl PakeState {
    /// Start SPAKE2. Returns the state and the message to send to the peer.
    pub fn start(role: Role, code: &str) -> (Self, Vec<u8>) {
        let pw = Password::new(normalise_code(code).as_bytes());
        let (inner, msg) = match role {
            Role::Initiator => Spake2::<Ed25519Group>::start_a(
                &pw,
                &SpakeId::new(ID_INITIATOR),
                &SpakeId::new(ID_RESPONDER),
            ),
            Role::Responder => Spake2::<Ed25519Group>::start_b(
                &pw,
                &SpakeId::new(ID_INITIATOR),
                &SpakeId::new(ID_RESPONDER),
            ),
        };
        (PakeState { role, inner }, msg)
    }

    /// Finish SPAKE2 with the peer's message and derive confirmation keys.
    pub fn finish(self, peer_msg: &[u8], binding: &Binding) -> Result<Confirmer> {
        let key = self
            .inner
            .finish(peer_msg)
            .map_err(|e| anyhow!("pairing exchange failed: {e:?}"))?;
        Ok(Confirmer {
            role: self.role,
            key,
            transcript: binding.transcript(),
        })
    }
}

/// Values that tie the pairing to this exact TLS session and these devices.
pub struct Binding {
    pub tls_exporter: [u8; 32],
    pub initiator_fp: Fingerprint,
    pub responder_fp: Fingerprint,
}

impl Binding {
    fn transcript(&self) -> Vec<u8> {
        let mut t = Vec::with_capacity(96);
        t.extend_from_slice(&self.tls_exporter);
        t.extend_from_slice(&self.initiator_fp.0);
        t.extend_from_slice(&self.responder_fp.0);
        t
    }
}

pub struct Confirmer {
    role: Role,
    key: Vec<u8>,
    transcript: Vec<u8>,
}

impl Confirmer {
    fn mac_for(&self, role: Role) -> HmacSha256 {
        let mut m = HmacSha256::new_from_slice(&self.key).expect("hmac takes any key length");
        m.update(match role {
            Role::Initiator => b"confirm-initiator",
            Role::Responder => b"confirm-responder",
        });
        m.update(&self.transcript);
        m
    }

    /// Our confirmation MAC, to send.
    pub fn own_mac(&self) -> Vec<u8> {
        self.mac_for(self.role).finalize().into_bytes().to_vec()
    }

    /// Check the peer's confirmation MAC (constant time).
    pub fn verify_peer(&self, mac: &[u8]) -> bool {
        let peer = match self.role {
            Role::Initiator => Role::Responder,
            Role::Responder => Role::Initiator,
        };
        self.mac_for(peer).verify_slice(mac).is_ok()
    }
}

/// Rate limit for incoming pairing attempts. Global rather than per address,
/// because an attacker on the LAN can change address freely.
#[derive(Debug, Default)]
pub struct PairGuard {
    failures: VecDeque<Instant>,
    locked_until: Option<Instant>,
    busy: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GuardDenied {
    Locked(Duration),
    Busy,
}

impl PairGuard {
    /// Try to start a pairing attempt. Only one may run at a time.
    pub fn begin(&mut self, now: Instant) -> Result<(), GuardDenied> {
        if let Some(until) = self.locked_until {
            if now < until {
                return Err(GuardDenied::Locked(until - now));
            }
            self.locked_until = None;
            self.failures.clear();
        }
        if self.busy {
            return Err(GuardDenied::Busy);
        }
        self.busy = true;
        Ok(())
    }

    pub fn succeeded(&mut self) {
        self.busy = false;
        self.failures.clear();
    }

    pub fn failed(&mut self, now: Instant) {
        self.busy = false;
        self.failures.push_back(now);
        while let Some(&t) = self.failures.front() {
            if now.duration_since(t) > FAILURE_WINDOW {
                self.failures.pop_front();
            } else {
                break;
            }
        }
        if self.failures.len() >= MAX_FAILURES {
            self.locked_until = Some(now + LOCKOUT);
        }
    }

    /// The attempt ended without a guess being made (peer left, timeout
    /// before any code was tried). Does not count as a failure.
    pub fn abandoned(&mut self) {
        self.busy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding {
        Binding {
            tls_exporter: [1; 32],
            initiator_fp: Fingerprint([2; 32]),
            responder_fp: Fingerprint([3; 32]),
        }
    }

    fn run(code_i: &str, code_r: &str, b_i: &Binding, b_r: &Binding) -> (bool, bool) {
        let (si, mi) = PakeState::start(Role::Initiator, code_i);
        let (sr, mr) = PakeState::start(Role::Responder, code_r);
        let ci = si.finish(&mr, b_i).unwrap();
        let cr = sr.finish(&mi, b_r).unwrap();
        (cr.verify_peer(&ci.own_mac()), ci.verify_peer(&cr.own_mac()))
    }

    #[test]
    fn same_code_confirms() {
        assert_eq!(
            run("123456", "123 456", &binding(), &binding()),
            (true, true)
        );
    }

    #[test]
    fn wrong_code_fails() {
        assert_eq!(
            run("123456", "654321", &binding(), &binding()),
            (false, false)
        );
    }

    #[test]
    fn different_tls_session_fails() {
        let mut other = binding();
        other.tls_exporter = [9; 32];
        assert_eq!(run("123456", "123456", &binding(), &other), (false, false));
    }

    #[test]
    fn codes_are_six_digits() {
        for _ in 0..100 {
            let c = new_code();
            assert_eq!(c.len(), 6);
            assert!(c.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn guard_locks_after_failures() {
        let mut g = PairGuard::default();
        let t0 = Instant::now();
        for _ in 0..MAX_FAILURES {
            g.begin(t0).unwrap();
            g.failed(t0);
        }
        assert!(matches!(g.begin(t0), Err(GuardDenied::Locked(_))));
        assert!(g.begin(t0 + LOCKOUT + Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn guard_allows_one_at_a_time() {
        let mut g = PairGuard::default();
        let t0 = Instant::now();
        g.begin(t0).unwrap();
        assert_eq!(g.begin(t0), Err(GuardDenied::Busy));
        g.abandoned();
        assert!(g.begin(t0).is_ok());
    }
}
