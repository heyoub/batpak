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
}

impl FlakeyDevice {
    pub fn create(size_bytes: u64) -> io::Result<Self> {
        let sectors = sectors_for(size_bytes)?;
        let suffix = unique_suffix();
        let root = std::env::temp_dir().join(format!("batpak-chaos-{suffix}"));
        let mount_point = root.join("mnt");
        let backing_file = root.join("backing.img");
        let device_name = format!("batpak-chaos-{suffix}");

        fs::create_dir_all(&mount_point)?;
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&backing_file)?
            .set_len(size_bytes)?;

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
        })
    }

    pub fn mount_ext4(&self) -> io::Result<()> {
        let mapper = self.mapper_path();
        run_privileged(
            "mkfs.ext4",
            [OsStr::new("-F"), OsStr::new("-q"), mapper.as_os_str()],
        )?;
        run_privileged(
            "mount",
            [
                OsStr::new("-o"),
                OsStr::new("sync"),
                mapper.as_os_str(),
                self.mount_point.as_os_str(),
            ],
        )?;
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
        for result in [
            self.unmount(),
            run_privileged(
                "dmsetup",
                [
                    OsStr::new("remove"),
                    OsStr::new("--force"),
                    OsStr::new(&self.device_name),
                ],
            )
            .map(drop),
            run_privileged("losetup", [OsStr::new("-d"), self.loop_path.as_os_str()]).map(drop),
            fs::remove_file(&self.backing_file),
            fs::remove_dir(&self.mount_point),
            remove_parent_dir(&self.backing_file),
        ] {
            if let Err(err) = result {
                first_error.get_or_insert(err);
            }
        }
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
