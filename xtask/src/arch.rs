﻿//! 平台相关的操作。

use crate::{
    command::{dir, download::wget, Cargo, CommandExt, Ext, Make, Qemu, Tar},
    XError, ALPINE_ROOTFS_VERSION, ALPINE_WEBSITE,
};
use dircpy::copy_dir;
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    os::unix,
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Clone, Copy)]
pub(crate) enum Arch {
    Riscv64,
    X86_64,
}

impl FromStr for Arch {
    type Err = XError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "riscv64" => Ok(Self::Riscv64),
            "x86_64" => Ok(Self::X86_64),
            _ => Err(XError::EnumParse {
                type_name: "Arch",
                value: s.into(),
            }),
        }
    }
}

impl Arch {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Arch::Riscv64 => "riscv64",
            Arch::X86_64 => "x86_64",
        }
    }

    /// 构造启动内存文件系统 rootfs。
    /// 对于 x86_64，这个文件系统可用于 libos 启动。
    /// 若设置 `clear`，将清除已存在的目录。
    pub fn make_rootfs(&self, clear: bool) {
        // 若已存在且不需要清空，可以直接退出
        let dir = self.rootfs();
        if dir.is_dir() && !clear {
            return;
        }
        // 下载压缩文件并解压
        let src = self.prebuild_rootfs();
        // 创建目标目录
        dir::clear(&dir).unwrap();
        fs::create_dir(dir.join("bin")).unwrap();
        fs::create_dir(dir.join("lib")).unwrap();
        // 拷贝 busybox
        fs::copy(src.join("bin/busybox"), dir.join("bin/busybox")).unwrap();
        // 拷贝 libc.so
        let libc_so = format!("lib/ld-musl-{arch}.so.1", arch = self.as_str());
        let so = match self {
            Arch::Riscv64 => src.join(&libc_so),
            Arch::X86_64 => PathBuf::from("prebuilt/linux/libc-libos.so"),
        };
        fs::copy(so, dir.join(libc_so)).unwrap();
        // 为常用功能建立符号链接
        const SH: &[&str] = &[
            "cat", "cp", "echo", "false", "grep", "gzip", "kill", "ln", "ls", "mkdir", "mv",
            "pidof", "ping", "ping6", "printenv", "ps", "pwd", "rm", "rmdir", "sh", "sleep",
            "stat", "tar", "touch", "true", "uname", "usleep", "watch",
        ];
        let bin = dir.join("bin");
        for sh in SH {
            unix::fs::symlink("busybox", bin.join(sh)).unwrap();
        }
    }

    /// 将 libc-test 放入 rootfs。
    pub fn put_libc_test(&self) {
        // 递归 rootfs
        self.make_rootfs(false);
        // 拷贝仓库
        let dir = self.libc_test();
        dir::rm(&dir).unwrap();
        copy_dir("libc-test", &dir).unwrap();
        // 编译
        fs::copy(dir.join("config.mak.def"), dir.join("config.mak")).unwrap();
        match self {
            Arch::Riscv64 => {
                Make::new(None)
                    .env("ARCH", self.as_str())
                    .env("CROSS_COMPILE", "riscv64-linux-musl-")
                    .env("PATH", riscv64_linux_musl_cross())
                    .current_dir(&dir)
                    .invoke();
                fs::copy(
                    self.target()
                        .join("rootfs/libc-test/functional/tls_align-static.exe"),
                    dir.join("src/functional/tls_align-static.exe"),
                )
                .unwrap();
            }
            Arch::X86_64 => {
                fs::OpenOptions::new()
                    .append(true)
                    .open(dir.join("config.mak"))
                    .unwrap()
                    .write_all(b"CC := musl-gcc\nAR := ar\nRANLIB := ranlib")
                    .unwrap();
                Make::new(None).current_dir(dir).invoke();
            }
        }
    }

    /// 将其他测试放入 rootfs。
    pub fn put_other_test(&self) {
        // 递归 rootfs
        self.make_rootfs(false);
        let rootfs = self.rootfs();
        match self {
            Arch::Riscv64 => {
                let target = self.target();
                copy_dir(target.join("rootfs/oscomp"), rootfs.join("oscomp")).unwrap();
            }
            Arch::X86_64 => {
                let bin = rootfs.join("bin");
                fs::read_dir("linux-syscall/test")
                    .unwrap()
                    .filter_map(|res| res.ok())
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().map_or(false, |ext| ext == OsStr::new("c")))
                    .for_each(|c| {
                        Ext::new("gcc")
                            .arg(&c)
                            .arg("-o")
                            .arg(bin.join(c.file_prefix().unwrap()))
                            .arg("-Wl,--dynamic-linker=/lib/ld-musl-x86_64.so.1")
                            .invoke();
                    });
            }
        }
    }

    /// 生成镜像。
    pub fn image(&self) {
        // 递归 rootfs
        self.make_rootfs(false);
        let arch_str = self.as_str();
        let image = match self {
            Arch::Riscv64 => {
                let rootfs = format!("rootfs/{arch_str}");
                let image = format!("zCore/{arch_str}.img");
                fuse(rootfs, &image);
                image
            }
            Arch::X86_64 => {
                let target = self.target();
                let rootfs = self.rootfs();
                fs::copy(
                    target.join("rootfs/lib/ld-musl-x86_64.so.1"),
                    rootfs.join("lib/ld-musl-x86_64.so.1"),
                )
                .unwrap();

                let image = format!("zCore/{arch_str}.img");
                fuse(&rootfs, &image);

                fs::copy(
                    "prebuilt/linux/libc-libos.so",
                    rootfs.join("lib/ld-musl-x86_64.so.1"),
                )
                .unwrap();

                image
            }
        };
        Qemu::img()
            .arg("resize")
            .args(&["-f", "raw"])
            .arg(image)
            .arg("+5M")
            .invoke();
    }

    /// 在 qemu 中启动。
    pub fn qemu(&self) {
        // 递归 image
        self.image();
        match self {
            Arch::Riscv64 => {
                Cargo::build()
                    .package("zcore")
                    .features(false, &["linux", "board-qemu"])
                    .target("zCore/riscv64.json")
                    .args(&["-Z", "build-std=core,alloc"])
                    .args(&["-Z", "build-std-features=compiler-builtins-mem"])
                    .release()
                    .invoke();
                Ext::new("rust-objcopy")
                    .arg("--binary-architecture=riscv64")
                    .arg("target/riscv64/release/zcore")
                    .arg("--strip-all")
                    .args(&["-O", "binary", "target/riscv64/release/zcore.bin"])
                    .invoke();
                Qemu::system(*self)
                    .args(&["-smp", "1"])
                    .args(&["-machine", "virt"])
                    .arg("-bios")
                    .arg(rustsbi_qemu())
                    .args(&["-m", "512M"])
                    .args(&["-serial", "mon:stdio"])
                    .args(&["-kernel", "target/riscv64/release/zcore.bin"])
                    .args(&["-initrd", "zCore/riscv64.img"])
                    .args(&["-append", "\"LOG=warn\""])
                    .args(&["-display", "none"])
                    .arg("-no-reboot")
                    .arg("-nographic")
                    .invoke();
            }
            Arch::X86_64 => todo!(),
        }
    }

    fn rootfs(&self) -> PathBuf {
        let mut path = PathBuf::new();
        path.push("rootfs");
        path.push(self.as_str());
        path
    }

    fn libc_test(&self) -> PathBuf {
        let mut path = self.rootfs();
        path.push("libc-test");
        path
    }

    fn origin(&self) -> PathBuf {
        let mut path = PathBuf::new();
        path.push("ignored");
        path.push("origin");
        path.push(self.as_str());
        path
    }

    fn target(&self) -> PathBuf {
        let mut path = PathBuf::new();
        path.push("ignored");
        path.push("target");
        path.push(self.as_str());
        path
    }

    /// 下载并解压 minirootfs。
    fn prebuild_rootfs(&self) -> PathBuf {
        // 构造压缩文件路径
        let file_name = match self {
            Arch::Riscv64 => "minirootfs.tar.xz",
            Arch::X86_64 => "minirootfs.tar.gz",
        };
        let tar = self.origin().join(file_name);
        // 若压缩文件不存在，需要下载
        if !tar.exists() {
            let url = match self {
                Arch::Riscv64 => String::from("https://github.com/rcore-os/libc-test-prebuilt/releases/download/0.1/prebuild.tar.xz"),
                Arch::X86_64 => format!("{ALPINE_WEBSITE}/x86_64/alpine-minirootfs-{ALPINE_ROOTFS_VERSION}-x86_64.tar.gz"),
            };
            wget(url, &tar);
        }
        // 解压到目标路径
        let dir = self.target().join("rootfs");
        dir::clear(&dir).unwrap();
        let mut tar = Tar::xf(&tar, Some(&dir));
        match self {
            Arch::Riscv64 => tar.args(&["--strip-components", "1"]).invoke(),
            Arch::X86_64 => tar.invoke(),
        }
        dir
    }
}

/// 下载 riscv64-musl 工具链。
fn riscv64_linux_musl_cross() -> OsString {
    const NAME: &str = "riscv64-linux-musl-cross";

    let origin = Arch::Riscv64.origin();
    let target = Arch::Riscv64.target();

    let tgz = origin.join(format!("{NAME}.tgz"));
    let dir = target.join(NAME);

    dir::rm(&dir).unwrap();
    wget(format!("https://musl.cc/{NAME}.tgz"), &tgz);
    Tar::xf(&tgz, Some(target)).invoke();

    // 将交叉工具链加入 PATH 环境变量
    let mut path = OsString::new();
    if let Ok(current) = std::env::var("PATH") {
        path.push(current);
        path.push(":");
    }
    path.push(std::env::current_dir().unwrap());
    path.push("/");
    path.push(dir);
    path.push("/bin");
    path
}

/// 制作镜像。
fn fuse(dir: impl AsRef<Path>, image: impl AsRef<Path>) {
    use rcore_fs::vfs::FileSystem;
    use rcore_fs_fuse::zip::zip_dir;
    use rcore_fs_sfs::SimpleFileSystem;
    use std::sync::{Arc, Mutex};

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(image)
        .expect("failed to open image");
    const MAX_SPACE: usize = 0x1000 * 0x1000 * 1024; // 1G
    let fs = SimpleFileSystem::create(Arc::new(Mutex::new(file)), MAX_SPACE)
        .expect("failed to create sfs");
    zip_dir(dir.as_ref(), fs.root_inode()).expect("failed to zip fs");
}

fn rustsbi_qemu() -> PathBuf {
    const NAME: &str = "rustsbi-qemu-release";

    let origin = Arch::Riscv64.origin();
    let target = Arch::Riscv64.target();

    let zip = origin.join(format!("{NAME}.zip"));
    let dir = target.join(NAME);
    let url =
        format!("https://github.com/rustsbi/rustsbi-qemu/releases/download/v0.1.1/{NAME}.zip");

    dir::rm(&dir).unwrap();
    wget(url, &zip);
    Ext::new("unzip").arg("-d").arg(&dir).arg(zip).invoke();

    dir.join("rustsbi-qemu.bin")
}
