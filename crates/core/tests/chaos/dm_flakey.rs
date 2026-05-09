use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const SECTOR_SIZE: u64 = 512;

pub struct FlakeyDevice {
    pub loop_path: PathBuf,
    pub device_name: String,
    pub mount_point: PathBuf,
    pub backing_file: PathBuf,
    root_dir: PathBuf,
    owns_backing: bool,
}

impl FlakeyDevice {
    pub fn create(size_bytes: u64) -> io::Result<Self> {
        let suffix = unique_suffix();
        let root = std::env::temp_dir().join(format!("batpak-chaos-{suffix}"));
        let backing_file = root.join("backing.img");
        Self::create_inner(&backing_file, size_bytes, true)
    }

    pub fn create_with_backing(backing: &Path, size_bytes: u64) -> io::Result<Self> {
        Self::create_inner(backing, size_bytes, false)
    }

    pub fn open_existing_backing(backing: &Path) -> io::Result<Self> {
        let sectors = backing_file_sectors(backing)?;
        Self::attach_existing(backing.to_path_buf(), sectors, false)
    }

    pub fn format_and_mount_ext4_with_sync(&self) -> io::Result<()> {
        self.format_ext4()?;
        self.mount_ext4([OsStr::new("-o"), OsStr::new("sync")])
    }

    pub fn format_and_mount_ext4_default(&self) -> io::Result<()> {
        self.format_ext4()?;
        self.mount_ext4(std::iter::empty::<&OsStr>())
    }

    pub fn mount_existing_ext4(&self) -> io::Result<()> {
        self.mount_ext4(std::iter::empty::<&OsStr>())
    }

    pub fn data_dir(&self) -> PathBuf {
        self.mount_point.join("data")
    }

    fn create_inner(backing: &Path, size_bytes: u64, owns_backing: bool) -> io::Result<Self> {
        let sectors = sectors_for(size_bytes)?;
        if let Some(parent) = backing.parent() {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(backing)?
            .set_len(size_bytes)?;

        Self::attach_existing(backing.to_path_buf(), sectors, owns_backing)
    }

    fn attach_existing(
        backing_file: PathBuf,
        sectors: u64,
        owns_backing: bool,
    ) -> io::Result<Self> {
        let suffix = unique_suffix();
        let root_dir = std::env::temp_dir().join(format!("batpak-chaos-{suffix}"));
        let mount_point = root_dir.join("mnt");
        let device_name = format!("batpak-chaos-{suffix}");

        fs::create_dir_all(&mount_point)?;

        let loop_output = run_privileged(
            "losetup",
            [
                OsStr::new("--find"),
                OsStr::new("--show"),
                backing_file.as_os_str(),
            ],
        )?;
        let loop_path = parse_stdout_path(&loop_output)?;

        let table = format!("0 {sectors} linear {} 0", loop_path.display());
        if let Err(err) = run_privileged(
            "dmsetup",
            [
                OsStr::new("create"),
                OsStr::new(&device_name),
                OsStr::new("--table"),
                OsStr::new(&table),
            ],
        ) {
            let _ = run_privileged("losetup", [OsStr::new("-d"), loop_path.as_os_str()]);
            return Err(err);
        }

        Ok(Self {
            loop_path,
            device_name,
            mount_point,
            backing_file,
            root_dir,
            owns_backing,
        })
    }

    fn format_ext4(&self) -> io::Result<()> {
        let mapper = self.mapper_path();
        run_privileged(
            "mkfs.ext4",
            [OsStr::new("-F"), OsStr::new("-q"), mapper.as_os_str()],
        )?;
        Ok(())
    }

    fn mount_ext4<I, S>(&self, options: I) -> io::Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mapper = self.mapper_path();
        let mut args = options
            .into_iter()
            .map(|arg| arg.as_ref().to_os_string())
            .collect::<Vec<_>>();
        args.push(mapper.into_os_string());
        args.push(self.mount_point.as_os_str().to_os_string());
        run_privileged("mount", args)?;
        self.chown_mount_to_current_user()?;
        Ok(())
    }

    fn chown_mount_to_current_user(&self) -> io::Result<()> {
        let owner = format!("{}:{}", current_id("-u")?, current_id("-g")?);
        run_privileged("chown", [OsStr::new(&owner), self.mount_point.as_os_str()])?;
        Ok(())
    }

    pub fn flip_to_error(&self) -> io::Result<()> {
        let sectors = backing_file_sectors(&self.backing_file)?;
        let table = format!("0 {sectors} error");
        run_privileged(
            "dmsetup",
            [OsStr::new("suspend"), OsStr::new(&self.device_name)],
        )?;
        let reload = run_privileged(
            "dmsetup",
            [
                OsStr::new("reload"),
                OsStr::new(&self.device_name),
                OsStr::new("--table"),
                OsStr::new(&table),
            ],
        );
        let resume = run_privileged(
            "dmsetup",
            [OsStr::new("resume"), OsStr::new(&self.device_name)],
        );
        reload?;
        resume?;
        Ok(())
    }

    pub fn unmount(&self) -> io::Result<()> {
        run_privileged("umount", [self.mount_point.as_os_str()])?;
        Ok(())
    }

    fn mapper_path(&self) -> PathBuf {
        PathBuf::from("/dev/mapper").join(&self.device_name)
    }

    fn teardown(&self) -> io::Result<()> {
        let mut first_error = None;
        let mut record = |result: io::Result<()>| {
            if let Err(err) = result {
                first_error.get_or_insert(err);
            }
        };

        record(self.unmount());
        record(
            run_privileged(
                "dmsetup",
                [
                    OsStr::new("remove"),
                    OsStr::new("--force"),
                    OsStr::new(&self.device_name),
                ],
            )
            .map(drop),
        );
        record(run_privileged("losetup", [OsStr::new("-d"), self.loop_path.as_os_str()]).map(drop));
        if self.owns_backing {
            record(fs::remove_file(&self.backing_file));
            if self.backing_file.parent() != Some(self.root_dir.as_path()) {
                record(remove_parent_dir(&self.backing_file));
            }
        }
        record(fs::remove_dir(&self.mount_point));
        record(fs::remove_dir(&self.root_dir));

        if let Some(err) = first_error {
            return Err(err);
        }
        Ok(())
    }
}

impl Drop for FlakeyDevice {
    fn drop(&mut self) {
        if let Err(err) = self.teardown() {
            eprintln!(
                "warning: dm-flakey teardown for {} was incomplete: {err}",
                self.device_name
            );
        }
    }
}

fn sectors_for(size_bytes: u64) -> io::Result<u64> {
    if size_bytes == 0 || !size_bytes.is_multiple_of(SECTOR_SIZE) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("device size {size_bytes} must be a non-zero multiple of {SECTOR_SIZE}"),
        ));
    }
    Ok(size_bytes / SECTOR_SIZE)
}

fn backing_file_sectors(path: &Path) -> io::Result<u64> {
    sectors_for(fs::metadata(path)?.len())
}

fn parse_stdout_path(output: &Output) -> io::Result<PathBuf> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "losetup returned an empty loop-device path",
        ));
    }
    Ok(PathBuf::from(trimmed))
}

fn run_privileged<I, S>(program: &str, args: I) -> io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect::<Vec<_>>();
    let mut command = privileged_command(program, &args);
    let description = format!("{command:?}");
    let output = command.output()?;
    if !output.status.success() {
        return Err(command_error(&description, &output));
    }
    Ok(output)
}

fn privileged_command(program: &str, args: &[OsString]) -> Command {
    if running_as_root() {
        let mut command = Command::new(program);
        command.args(args);
        return command;
    }

    let mut command = Command::new("sudo");
    command.arg("--non-interactive").arg(program).args(args);
    command
}

fn running_as_root() -> bool {
    let Ok(output) = Command::new("id").arg("-u").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).trim() == "0"
}

fn current_id(flag: &str) -> io::Result<String> {
    let output = Command::new("id").arg(flag).output()?;
    if !output.status.success() {
        return Err(command_error(&format!("id {flag}"), &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn command_error(description: &str, output: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    io::Error::other(format!(
        "{description} failed with status {} stdout={stdout:?} stderr={stderr:?}",
        output.status
    ))
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn remove_parent_dir(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => fs::remove_dir(parent),
        None => Ok(()),
    }
}
