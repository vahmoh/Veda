// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A program: linking plus the optimised code of both stages.
//!
//! [`link`] is what `glLinkProgram` does: it links the shaders' interfaces
//! ([`crate::link`]), builds each stage's SSA form ([`crate::ir::build`]),
//! optimises it and checks it. The result keeps everything the back ends
//! and the OpenGL ES API need, and nothing of the shaders themselves, which
//! may change or go away after linking.
//!
//! What the back ends are given is bounded: a stage of more than
//! [`MAX_VALUES`] values once optimised fails to link (so that the
//! interpreter's register numbers, 16 bits, are never too few), and every
//! uniform, block, sampler, input and output the code names is checked to
//! be the program's, so that neither back end reads outside the storage it
//! is given.

use alloc::string::String;

use crate::builtins::Limits;
use crate::hir::Shader;
use crate::ir::{self, Func};
use crate::link::{self, Bindings, Linked, StageLayout};

/// The most values a stage's optimised code may compute. The interpreter
/// needs at most two registers a value (one for the value, one while it is
/// copied into a φ), and numbers its registers with 16 bits.
pub const MAX_VALUES: usize = (1 << 15) - 1;

/// A linked program.
#[derive(Debug)]
pub struct Program {
    pub linked: Linked,
    pub vertex: Func,
    pub fragment: Func,
}

/// The result of linking.
pub struct LinkResult {
    pub program: Option<Program>,
    /// Errors, as `glGetProgramInfoLog` returns them.
    pub log: String,
}

/// Links a vertex and a fragment shader into a program.
pub fn link(vs: &Shader, fs: &Shader, bindings: &Bindings, limits: &Limits) -> LinkResult {
    let r = link::link(vs, fs, bindings, limits);
    let Some(linked) = r.linked else { return LinkResult { program: None, log: r.log } };
    let vertex = stage(vs, &linked.vertex, &linked, limits, "vertex");
    let fragment = stage(fs, &linked.fragment, &linked, limits, "fragment");
    match (vertex, fragment) {
        (Ok(vertex), Ok(fragment)) => LinkResult { program: Some(Program { linked, vertex, fragment }), log: r.log },
        (v, f) => {
            let mut log = r.log;
            for e in [v.err(), f.err()].into_iter().flatten() {
                log.push_str(&e);
            }
            LinkResult { program: None, log }
        }
    }
}

/// Builds, optimises and checks one stage; the log's line if it fails.
fn stage(s: &Shader, layout: &StageLayout, linked: &Linked, limits: &Limits, what: &str) -> Result<Func, String> {
    let mut f = ir::build::build(s, layout, linked).map_err(|e| alloc::format!("ERROR: the {what} shader: {e}\n"))?;
    ir::opt::optimize(&mut f);
    let values: usize = f.blocks.iter().flat_map(|b| &b.insts).map(|&i| f.inst(i).results.len()).sum();
    if values > MAX_VALUES {
        return Err(alloc::format!(
            "ERROR: the {what} shader is too large ({values} values once optimised; at most {MAX_VALUES})\n"
        ));
    }
    // A failure here is a compiler bug; refuse the program rather than run
    // code that breaks the IR's rules or reads outside its storage.
    ir::verify::verify(&f)
        .and_then(|()| ir::verify::bounds(&f, linked, limits))
        .map_err(|e| alloc::format!("ERROR: internal compiler error in the {what} shader: {e}\n"))?;
    Ok(f)
}
