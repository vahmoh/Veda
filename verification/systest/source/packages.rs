// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The packages service: the system image's repository is there, valid and
//! empty, at every start; packages install from a repository the test adds
//! (the image's test repositories, signed by their own key), with what they
//! depend on, and run from `/home/packages`, which only the service changes;
//! what conflicts or needs a newer system is refused, and changes nothing;
//! the files installed check out; and packages go again with what was
//! installed only for them.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use proto::fs::FsError;
use proto::packages::{Action, Failure, PackageError, Plan, RepositoryChange, Request, packages};
use rt::object::Vmo;
use rt::println;

use crate::{TestResult, check, vfs_client};

/// The test repositories' public key, and their first repository.
const TEST_KEY: &str = "/system/tests/packages/test.pub";
const TEST_ONE: &str = "file:///system/tests/packages/one";
/// How long a call may take: a change unpacks packages, which takes a while
/// under emulation.
const CALL_NS: u64 = 120_000_000_000;

fn err<E: core::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn answered<T>(r: Result<Result<T, Failure>, ipc::IpcError>) -> Result<T, String> {
    r.map_err(err)?.map_err(|f| alloc::format!("{:?}: {f}", f.error))
}

/// The steps of `plan`, as `install hello 1.0`.
fn steps(plan: &Plan) -> Vec<String> {
    plan.steps
        .iter()
        .map(|s| match s.action {
            Action::Install => alloc::format!("install {} {}", s.name, s.to),
            Action::Remove => alloc::format!("remove {} {}", s.name, s.from),
            Action::Upgrade => alloc::format!("upgrade {} {}", s.name, s.to),
            Action::Downgrade => alloc::format!("downgrade {} {}", s.name, s.to),
        })
        .collect()
}

/// Plans `request` and carries it out.
fn change(p: &packages::Client, request: Request) -> Result<proto::packages::Outcome, Failure> {
    let plan = p.plan(request.clone()).map_err(|e| Failure::new(PackageError::Unavailable, e.to_string()))??;
    p.apply(request, plan.digest, None).map_err(|e| Failure::new(PackageError::Unavailable, e.to_string()))?
}

fn install(names: &[&str]) -> Request {
    Request::Install { packages: names.iter().map(|n| n.to_string()).collect() }
}

pub fn test_packages() -> TestResult {
    let ch = proto::connect(packages::NAME).map_err(|e| alloc::format!("connect packages: {e:?}"))?;
    let p = packages::Client::new(ch);
    p.set_timeout(CALL_NS);
    let repositories = p.repositories().map_err(err)?;
    let system = repositories.iter().find(|r| r.name == "system").ok_or("no system repository")?;
    println!(
        "packages: the system's repository: {} packages, serial {}, {}",
        system.packages, system.serial, system.url
    );
    check(system.system && system.enabled && system.problem.is_empty(), "the system's repository is usable")?;
    check(system.packages == 0 && system.serial == 1, "the system's repository is the image's, empty")?;
    check(p.status().map_err(err)?.problem.is_empty(), "what is installed reads")?;
    let nothing = p.plan(install(&["nothing-here"])).map_err(err)?;
    check(matches!(nothing, Err(Failure { error: PackageError::Unsatisfiable, .. })), "nothing installs from nowhere")?;

    let fs = vfs_client()?;
    let Ok(Ok((vmo, len))) = fs.read_file(TEST_KEY.into()) else {
        println!("packages: no test repositories in this image (no C test programs were built)");
        return Ok(());
    };
    let mut key = vec![0u8; len as usize];
    vmo.read(0, &mut key).map_err(err)?;
    let key = String::from_utf8_lossy(&key).trim().to_string();
    let add =
        RepositoryChange::Add { name: "test".into(), title: String::new(), url: TEST_ONE.into(), keys: vec![key] };
    answered(p.configure(add))?;
    let updated = answered(p.update(None))?;
    check(updated.iter().all(|r| r.problem.is_empty()), "every repository updates")?;
    check(updated.iter().any(|r| r.repository == "test" && r.packages == 4), "the test repository offers its four")?;

    // Installed with what it depends on.
    let plan = answered(p.plan(install(&["hello"])))?;
    check(steps(&plan) == ["install hello 1.0", "install libgreet 1.0"], "hello comes with libgreet")?;
    let outcome = answered(p.apply(install(&["hello"]), plan.digest, None))?;
    check(outcome.installed == 2, "two packages installed")?;
    // Its program runs from where everyone reads it, which no one but the
    // packages service changes.
    let (code, out) = crate::posix::run_c("/home/packages/bin/hello", &[], "/tmp")?;
    check(code == 42 && out.contains("Hello from C on Veda!"), "the installed program runs")?;
    let st = fs.stat("/home/packages/bin/hello".into()).map_err(err)?.map_err(err)?;
    check(st.executable && st.read_only, "the installed program is read-only to others")?;
    let planted = Vmo::create(4).map_err(err)?;
    let refused = |r: Result<Result<(), FsError>, ipc::IpcError>| r.is_ok_and(|r| r == Err(FsError::Denied));
    check(refused(fs.write_file("/home/packages/bin/planted".into(), planted, 4)), "no file is planted there")?;
    check(refused(fs.remove("/home/packages/bin/hello".into())), "no file is removed from there")?;
    check(refused(fs.rename("/home/packages/bin/hello".into(), "/tmp/hello".into())), "nor moved out")?;
    check(refused(fs.remove("/home/packages".into())), "the directory stays")?;

    // A file of another package's, or a newer system: refused.
    let clash = change(&p, install(&["clash"]));
    check(matches!(clash, Err(Failure { error: PackageError::Conflict, .. })), "clash takes no file of libgreet's")?;
    let toonew = answered(p.plan(install(&["toonew"])));
    check(toonew.as_ref().is_err_and(|e| e.contains("veda (>= 99)")), "toonew needs a newer system")?;
    let report = answered(p.verify(String::new()))?;
    check(report.packages == 2 && report.problems.is_empty() && report.more == 0, "the files installed check out")?;

    // Removed, with what was installed only for it.
    let plan = answered(p.plan(Request::Remove { packages: vec!["hello".into()] }))?;
    check(steps(&plan) == ["remove hello 1.0", "remove libgreet 1.0"], "libgreet goes with hello")?;
    let outcome = answered(p.apply(Request::Remove { packages: vec!["hello".into()] }, plan.digest, None))?;
    check(outcome.removed == 2, "two packages removed")?;
    let gone = fs.stat("/home/packages/bin/hello".into()).map_err(err)?;
    check(gone == Err(FsError::NotFound), "the program is gone")?;
    answered(p.configure(RepositoryChange::Remove { name: "test".into() }))
}
