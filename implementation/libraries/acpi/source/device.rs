// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a device in the namespace is, and what it uses: its identification
//! objects (`_HID`, `_CID`, `_UID`, `_ADR`, `_SUB`, `_STA`) and its
//! current resources (`_CRS`).
//!
//! Each of this module's questions (a device's identity, the PCI root
//! bridges, where an interrupt goes) is answered by evaluations that spend
//! from one budget together, however many devices it looks at.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

use crate::Memory;
use crate::aml::{self, Namespace, Question};
use crate::name::Path;
use crate::resource::{self, Resource, ResourceError};
use crate::value::Value;

/// `_STA` bits: present, enabled, shown, functioning.
pub const STA_PRESENT: u64 = 1;
/// What a device without `_STA` is.
pub const STA_DEFAULT: u64 = 0xF;

/// A device's identification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    /// Hardware id: `CSC3551`, `INTC1055`, `PNP0A08`.
    pub hid: Option<String>,
    /// Compatible ids.
    pub cids: Vec<String>,
    /// Unique id, as text.
    pub uid: Option<String>,
    /// Address on its parent bus (PCI: device << 16 | function).
    pub adr: Option<u64>,
    /// Subsystem id: `10431F62`.
    pub sub: Option<String>,
    /// `_STA` ([`STA_DEFAULT`] if the device has none or it fails).
    pub status: u64,
}

impl Identity {
    /// Whether the device has `id` as its hardware or a compatible id.
    pub fn is(&self, id: &str) -> bool {
        self.hid.as_deref() == Some(id) || self.cids.iter().any(|c| c == id)
    }

    pub fn present(&self) -> bool {
        self.status & STA_PRESENT != 0
    }
}

/// Decodes a compressed EISA id (`PNP0A08`) as AML holds it.
pub fn eisa_id(value: u64) -> String {
    let v = (value as u32).swap_bytes();
    let letter = |shift: u32| (b'@' + (v >> shift & 0x1F) as u8) as char;
    format!("{}{}{}{:04X}", letter(26), letter(21), letter(16), v & 0xFFFF)
}

/// An id: a string, or a compressed EISA id.
fn id(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Integer(i) => Some(eisa_id(*i)),
        _ => None,
    }
}

/// A device's hardware id and compatible ids only (evaluating nothing
/// else), for finding devices of a kind.
pub fn ids(ns: &Namespace, device: &Path, memory: &dyn Memory) -> Vec<String> {
    ids_of(&Question::new(ns, memory), device)
}

fn ids_of(q: &Question, device: &Path) -> Vec<String> {
    let get = |name: &str| q.child(device, name).and_then(Result::ok);
    let mut ids: Vec<String> = get("_HID").as_ref().and_then(id).into_iter().collect();
    match get("_CID") {
        Some(Value::Package(items)) => ids.extend(items.iter().filter_map(id)),
        Some(v) => ids.extend(id(&v)),
        None => {}
    }
    ids
}

/// Reads a device's identification objects. Ones that are missing or fail
/// are left out.
pub fn identify(ns: &Namespace, device: &Path, memory: &dyn Memory) -> Identity {
    identity_of(&Question::new(ns, memory), device)
}

fn identity_of(q: &Question, device: &Path) -> Identity {
    let get = |name: &str| q.child(device, name).and_then(Result::ok);
    let cids = match get("_CID") {
        Some(Value::Package(items)) => items.iter().filter_map(id).collect(),
        Some(v) => id(&v).into_iter().collect(),
        None => Vec::new(),
    };
    Identity {
        hid: get("_HID").as_ref().and_then(id),
        cids,
        uid: get("_UID").and_then(|v| match v {
            Value::Integer(i) => Some(format!("{}", i)),
            Value::String(s) => Some(s),
            _ => None,
        }),
        adr: get("_ADR").and_then(|v| v.as_integer()),
        sub: get("_SUB").and_then(|v| v.as_str().map(String::from)),
        status: match q.child(device, "_STA") {
            Some(Ok(v)) => v.as_integer().unwrap_or(STA_DEFAULT),
            _ => STA_DEFAULT,
        },
    }
}

/// Why a device's resources could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourcesError {
    /// The device has no `_CRS`.
    None,
    Aml(aml::Error),
    /// `_CRS` returned something other than a buffer.
    NotABuffer,
    Template(ResourceError),
}

impl fmt::Display for ResourcesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourcesError::None => f.write_str("the device has no _CRS"),
            ResourcesError::Aml(e) => write!(f, "_CRS: {}", e),
            ResourcesError::NotABuffer => f.write_str("_CRS returned no resource template"),
            ResourcesError::Template(e) => write!(f, "_CRS: {}", e),
        }
    }
}

/// The resources a device currently uses (`_CRS`).
pub fn resources(ns: &Namespace, device: &Path, memory: &dyn Memory) -> Result<Vec<Resource>, ResourcesError> {
    resources_of(&Question::new(ns, memory), device)
}

fn resources_of(q: &Question, device: &Path) -> Result<Vec<Resource>, ResourcesError> {
    let value = q.child(device, "_CRS").ok_or(ResourcesError::None)?.map_err(ResourcesError::Aml)?;
    let template = value.as_buffer().ok_or(ResourcesError::NotABuffer)?;
    resource::parse(template).map_err(ResourcesError::Template)
}

/// The devices directly below `path` (in the namespace, not only on a
/// bus).
pub fn children(ns: &Namespace, path: &Path) -> Vec<Path> {
    ns.children(path).filter(|(_, o)| matches!(o, aml::Object::Device)).map(|(p, _)| p.clone()).collect()
}

/// The PCI root bridges (`PNP0A08`, `PNP0A03`): their bus number (`_BBN`,
/// 0 without) and path. Only the devices' ids are evaluated.
pub fn pci_roots(ns: &Namespace, memory: &dyn Memory) -> Vec<(u8, Path)> {
    let q = Question::new(ns, memory);
    let mut roots = Vec::new();
    for d in ns.devices() {
        if ids_of(&q, d).iter().any(|i| i == "PNP0A08" || i == "PNP0A03") {
            let bus = q.child(d, "_BBN").and_then(Result::ok).and_then(|v| v.as_integer());
            roots.push((bus.unwrap_or(0) as u8, d.clone()));
        }
    }
    roots
}

/// The memory windows PCI root bridge `root` forwards to its buses (end
/// exclusive): the memory ranges its `_CRS` produces, but empty ones (the
/// firmware's way of leaving one out) and translated ones, above the legacy
/// area below 1 MiB.
pub fn pci_root_windows(ns: &Namespace, root: &Path, memory: &dyn Memory) -> Result<Vec<Range<u64>>, ResourcesError> {
    Ok(resources(ns, root, memory)?
        .into_iter()
        .filter_map(|r| match r {
            Resource::Window { kind: 0, min, max, translation: 0, length } if length > 0 && min >= 0x10_0000 => {
                (max > min).then_some(min..max.checked_add(1)?)
            }
            _ => None,
        })
        .collect())
}

/// The memory a motherboard device reserves (end exclusive), or why its
/// `_CRS` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reserved {
    pub device: Path,
    pub memory: Result<Vec<Range<u64>>, ResourcesError>,
}

/// The memory the firmware reserves for the motherboard, where no BAR may
/// go: what each of its `PNP0C01` and `PNP0C02` devices uses.
pub fn motherboard_memory(ns: &Namespace, memory: &dyn Memory) -> Vec<Reserved> {
    let ranges = |r: Resource| match r {
        Resource::Memory { base, length, .. } if length > 0 => Some(base..base.checked_add(length)?),
        Resource::Window { kind: 0, min, max, length, .. } if length > 0 && max > min => Some(min..max.checked_add(1)?),
        _ => None,
    };
    let q = Question::new(ns, memory);
    ns.devices()
        .filter(|d| ids_of(&q, d).iter().any(|i| i == "PNP0C01" || i == "PNP0C02"))
        .map(|d| {
            let memory = match resources_of(&q, d) {
                Ok(r) => Ok(r.into_iter().filter_map(ranges).collect()),
                Err(ResourcesError::None) => Ok(Vec::new()),
                Err(e) => Err(e),
            };
            Reserved { device: d.clone(), memory }
        })
        .collect()
}

/// The device that describes PCI function `bus:slot.function`: the root
/// bridge's child whose `_ADR` is the function's (or any function of the
/// slot's). Devices on a root bus only.
pub fn pci_companion(
    ns: &Namespace,
    roots: &[(u8, Path)],
    (bus, slot, function): (u8, u8, u8),
    memory: &dyn Memory,
) -> Option<Path> {
    let (_, root) = roots.iter().find(|(b, _)| *b == bus)?;
    let exact = (slot as u64) << 16 | function as u64;
    let any_function = (slot as u64) << 16 | 0xFFFF;
    let q = Question::new(ns, memory);
    children(ns, root).into_iter().find(|d| {
        let adr = q.child(d, "_ADR").and_then(Result::ok).and_then(|v| v.as_integer());
        adr == Some(exact) || adr == Some(any_function)
    })
}

/// Where a PCI function's INTx goes: an interrupt controller's input (a
/// GSI), and how it signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntxRoute {
    pub gsi: u32,
    pub level: bool,
    pub active_low: bool,
}

/// Where INTx `pin` (1 for INTA# to 4) of the functions in `slot` of a
/// root bus goes, as the root bridge's `_PRT` says with the interrupt
/// model set to the I/O APICs' (`\_PIC (1)`, as an OS that uses them
/// calls first): a GSI it names (level-triggered, active low), or the one
/// an interrupt link device's `_CRS` holds. Devices on a root bus only.
pub fn pci_interrupt(
    ns: &Namespace,
    roots: &[(u8, Path)],
    (bus, slot): (u8, u8),
    pin: u8,
    memory: &dyn Memory,
) -> Option<IntxRoute> {
    let (_, root) = roots.iter().find(|(b, _)| *b == bus)?;
    let pic = Path::parse("\\_PIC")?;
    let prt = root.join("_PRT")?;
    // `_PRT` numbers INTA# 0.
    let route_pin = (pin as u64).checked_sub(1)?;
    let q = Question::new(ns, memory);
    let Value::Package(routes) = q.evaluate_after(&pic, &[Value::Integer(1)], &prt, &[]).ok()? else {
        return None;
    };
    for route in routes {
        let Value::Package(r) = route else { continue };
        let [address, pin, source, index] = r.as_slice() else { continue };
        if address.as_integer().map(|a| a >> 16) != Some(slot as u64) || pin.as_integer() != Some(route_pin) {
            continue;
        }
        let index = index.as_integer().unwrap_or(0);
        return match source {
            Value::Reference(link) => link_interrupt(&q, link, usize::try_from(index).ok()?),
            v if v.as_integer() == Some(0) => {
                Some(IntxRoute { gsi: u32::try_from(index).ok()?, level: true, active_low: true })
            }
            _ => None,
        };
    }
    None
}

/// The `index`-th interrupt an interrupt link device's `_CRS` holds.
fn link_interrupt(q: &Question, link: &Path, index: usize) -> Option<IntxRoute> {
    resources_of(q, link).ok()?.into_iter().find_map(|r| match r {
        Resource::Irq { irqs, edge, active_low, .. } => {
            Some(IntxRoute { gsi: *irqs.get(index)?, level: !edge, active_low })
        }
        _ => None,
    })
}

/// A device as its driver sees it: what it is and what it uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    pub path: Path,
    pub identity: Identity,
    pub resources: Result<Vec<Resource>, ResourcesError>,
}

impl Described {
    /// Its `index`-th GPIO pin, counting the pins of its connections
    /// (`GpioIo` and `GpioInt`) in order: the controller, resolved in the
    /// namespace, the pin, and the connection it is of.
    pub fn gpio(&self, ns: &Namespace, index: usize) -> Option<(Path, u16, &crate::resource::Gpio)> {
        let (gpio, pin) = self
            .resources
            .as_ref()
            .ok()?
            .iter()
            .filter_map(|r| match r {
                Resource::Gpio(g) => Some(g),
                _ => None,
            })
            .flat_map(|g| g.pins.iter().map(move |&p| (g, p)))
            .nth(index)?;
        let name = crate::name::NameString::parse(&gpio.controller)?;
        Some((ns.resolve(&self.path, &name)?, pin, gpio))
    }
}

/// The present devices directly below `path`, described. A device without
/// `_CRS` simply uses nothing.
pub fn describe_children(ns: &Namespace, path: &Path, memory: &dyn Memory) -> Vec<Described> {
    let q = Question::new(ns, memory);
    children(ns, path)
        .into_iter()
        .map(|p| Described {
            identity: identity_of(&q, &p),
            resources: match resources_of(&q, &p) {
                Err(ResourcesError::None) => Ok(Vec::new()),
                r => r,
            },
            path: p,
        })
        .filter(|d| d.identity.present())
        .collect()
}
