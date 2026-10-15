// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Initialising the devices once the tables are loaded, as an operating
//! system does, in ACPICA's order (which the firmware is written for):
//! `\_INI` (which Windows runs, and ACPICA for it), `\_SB._INI`, then each
//! device, processor and thermal zone that has an `_INI` in it or below
//! it, parents first and siblings in the order they were defined: its
//! `_STA`, and its `_INI` if that says it is present. The devices below one
//! neither present nor functioning are not looked at; below one that is
//! functioning but not present, they are.
//!
//! A firmware's `_INI` methods finish describing their devices: a laptop's
//! touchpad (Intel's reference code for it, which many laptops have) is
//! defined with a placeholder `_HID` and resource templates whose I2C
//! address and GPIO pin its `_INI` fills in, from the firmware's settings,
//! through buffer fields over them. One evaluation runs them all, and what
//! it stores becomes what every later evaluation starts from; the
//! firmware's memory is still not written.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use super::eval::Evaluator;
use super::{Error, Initialized, LOAD_BYTES, LOAD_STEPS, Namespace, Object};
use crate::Memory;
use crate::name::Path;

/// `_STA` bits: present, functioning.
const PRESENT: u64 = 1;
const FUNCTIONING: u64 = 1 << 3;

impl Initialized {
    fn failed(&mut self, path: &Path, why: Error) {
        self.failures += 1;
        if self.failed.len() < 16 {
            self.failed.push((path.clone(), why));
        }
    }
}

impl Namespace {
    /// Initialises the devices (see the module's overview), once the
    /// tables are loaded. A method that fails is counted and passed over,
    /// as ACPICA does.
    pub fn initialize(&mut self, memory: &dyn Memory) -> Initialized {
        let mut done = Initialized::default();
        let written = {
            let mut ev = Evaluator::with_limits(self, memory, &self.bound, LOAD_STEPS, LOAD_BYTES);
            let sb = Path::root().join("_SB").unwrap_or_default();
            self.run_ini(&mut ev, &Path::root(), &mut done);
            self.run_ini(&mut ev, &sb, &mut done);
            let mut skipping: Option<&Path> = None;
            for device in self.initialization_order() {
                if skipping.is_some_and(|s| device.0.starts_with(&s.0)) {
                    continue;
                }
                skipping = None;
                let status = match device.join("_STA") {
                    Some(sta) if self.objects.contains_key(&sta) => {
                        match ev
                            .evaluate(&sta, Vec::new())
                            .and_then(|v| v.as_integer().ok_or(Error::Type("an integer")))
                        {
                            Ok(status) => status,
                            Err(why) => {
                                // Its children are still looked at.
                                done.failed(&sta, why);
                                continue;
                            }
                        }
                    }
                    _ => PRESENT | FUNCTIONING,
                };
                if status & PRESENT == 0 {
                    if status & FUNCTIONING == 0 {
                        skipping = Some(device);
                    }
                    continue;
                }
                if *device != sb {
                    self.run_ini(&mut ev, device, &mut done);
                }
            }
            ev.into_written()
        };
        self.initial.extend(written);
        done
    }

    /// Runs `device`'s `_INI`, if it has one.
    fn run_ini(&self, ev: &mut Evaluator, device: &Path, done: &mut Initialized) {
        let Some(ini) = device.join("_INI") else { return };
        if !matches!(self.objects.get(&ini), Some(Object::Method { .. })) {
            return;
        }
        match ev.evaluate(&ini, Vec::new()) {
            Ok(_) => done.ran += 1,
            Err(why) => done.failed(&ini, why),
        }
    }

    /// The devices, processors and thermal zones (and `\_SB`, `\_TZ`) whose
    /// `_STA` and `_INI` initialisation runs: those with an `_INI` in them
    /// or below them, parents first, siblings in the order they were
    /// defined.
    fn initialization_order(&self) -> Vec<&Path> {
        let mut with_ini = BTreeSet::new();
        for device in self.order.keys() {
            let has =
                device.join("_INI").is_some_and(|ini| matches!(self.objects.get(&ini), Some(Object::Method { .. })));
            if has {
                let mut at = Some(device.clone());
                while let Some(p) = at {
                    at = p.parent();
                    with_ini.insert(p);
                }
            }
        }
        // Each device's place: the order of definition of each device it is
        // in, and its own.
        let mut keyed: Vec<(Vec<u32>, &Path)> = self
            .order
            .keys()
            .filter(|device| with_ini.contains(*device))
            .map(|device| {
                let key = (1..=device.0.len())
                    .filter_map(|n| self.order.get(&Path(device.0[..n].to_vec())).copied())
                    .collect();
                (key, device)
            })
            .collect();
        keyed.sort();
        keyed.into_iter().map(|(_, device)| device).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_are_initialised_parents_first_in_the_order_they_were_defined() {
        let path = |p: &str| Path::parse(p).unwrap();
        let mut ns = Namespace::new();
        let method = || Object::Method { args: 0, code: super::super::Code { table: 0, start: 0, end: 0 } };
        // Devices defined in this order: B, A, A.Y, B.X, A.Y.Z, C; with an
        // _INI: B.X, A.Y.Z and A.
        for (i, p) in ["\\_SB.B", "\\_SB.A", "\\_SB.A.Y", "\\_SB.B.X", "\\_SB.A.Y.Z", "\\_SB.C"].iter().enumerate() {
            ns.define(path(p), Object::Device);
            assert_eq!(ns.order[&path(p)], 2 + i as u32);
        }
        for p in ["\\_SB.B.X._INI", "\\_SB.A.Y.Z._INI", "\\_SB.A._INI"] {
            ns.define(path(p), method());
        }
        let order: Vec<Path> = ns.initialization_order().into_iter().cloned().collect();
        // C has no _INI below it; \_SB is first, as each device's parent.
        let expected = ["\\_SB", "\\_SB.B", "\\_SB.B.X", "\\_SB.A", "\\_SB.A.Y", "\\_SB.A.Y.Z"];
        assert_eq!(order, expected.map(path));
    }
}
