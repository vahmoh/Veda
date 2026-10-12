//! Starting a decoder: its program read once, then a process with nothing
//! but its control channel (no registry, so no file, service or network but
//! what it is sent), and a client to speak `vproto::media` with it.

use alloc::format;
use alloc::string::String;

use vabi::map_flags;
use vproto::fs::{FsError, vfs};
use vproto::media::{self, media::Client};
use vrt::object::{Channel, Process};
use vrt::process::Spawn;
use vrt::vm::Mapping;

/// The decoder's program, read once (a read-only mapping of the file
/// system's copy).
pub struct Image {
    path: String,
    map: Mapping,
}

impl Image {
    /// Reads the program at `path`.
    pub fn read(vfs: &vfs::Client, path: &str) -> Result<Image, String> {
        let (vmo, size) = match vfs.read_file(path.into()) {
            Ok(Ok(r)) => r,
            Ok(Err(FsError::NotFound)) if path == crate::engine::DECODER => {
                return Err(format!("{path} is not installed (the image was built without the C toolchain)"));
            }
            Ok(Err(FsError::NotFound)) => return Err(format!("{path} does not exist")),
            Ok(Err(e)) => return Err(format!("{path}: {e}")),
            Err(e) => return Err(format!("{path}: {e:?}")),
        };
        let map = Mapping::new(vmo, size as usize, map_flags::READ).map_err(|e| format!("{path}: {e:?}"))?;
        Ok(Image { path: path.into(), map })
    }
}

/// A running decoder, killed when dropped.
pub struct Decoder {
    pub client: Client,
    pub process: Process,
    pub build: String,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.process.kill_tree();
    }
}

/// Starts a decoder from `image` as process `name`; each call waits at most
/// `timeout_ns` for its answer.
pub fn start(image: &Image, name: &'static str, timeout_ns: u64) -> Result<Decoder, String> {
    let (ours, theirs) = Channel::create().map_err(|e| format!("{e:?}"))?;
    // SAFETY: a read-only mapping of the program, alive while it is loaded.
    let process = Spawn::new(name)
        .path(&image.path)
        .handle(media::CONTROL_ROLE, theirs.into_handle())
        .start(unsafe { image.map.as_slice() })
        .map_err(|e| format!("{}: {e}", image.path))?;
    let client = Client::new(ours);
    client.set_timeout(timeout_ns);
    let hello = client.hello(media::VERSION).map_err(|e| format!("the decoder does not answer ({e:?})"))?;
    if hello.version != media::VERSION {
        return Err(format!("the decoder speaks version {}, not {}", hello.version, media::VERSION));
    }
    Ok(Decoder { client, process, build: hello.build })
}
