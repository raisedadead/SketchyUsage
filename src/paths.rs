use std::{
    fs::{self, DirBuilder},
    io,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
};

#[derive(Clone, Debug)]
pub struct Paths {
    pub dir: PathBuf,
    pub state: PathBuf,
    pub lock: PathBuf,
    pub socket: PathBuf,
    pub heartbeat: PathBuf,
}

impl Paths {
    pub fn in_dir(dir: PathBuf) -> Self {
        Self {
            state: dir.join("state.json"),
            lock: dir.join("serve.lock"),
            socket: dir.join("serve.sock"),
            heartbeat: dir.join("heartbeat"),
            dir,
        }
    }

    pub fn from_env() -> io::Result<Self> {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .ok_or_else(|| io::Error::other("neither XDG_CACHE_HOME nor HOME is set"))?;
        Ok(Self::in_dir(cache.join("sketchyusage")))
    }

    pub fn create(&self) -> io::Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))
    }
}
