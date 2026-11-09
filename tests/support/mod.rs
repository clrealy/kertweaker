//! A stub kernel tree plus a stub `make`, so the build pipeline can be tested
//! end to end in under a second without compiling a real kernel.

use std::fs;
use std::path::{Path, PathBuf};

pub struct Fixture {
    pub root: PathBuf,
    pub tree: PathBuf,
    pub workdir: PathBuf,
    pub make: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A stub `make` that behaves enough like kbuild to exercise every stage: it
/// writes a `.config`, "resolves" it the way Kconfig would (dropping a symbol
/// that does not exist and forcing another off), emits CC/LD progress lines and
/// produces artifacts.
const FAKE_MAKE: &str = r##"#!/bin/sh
# Stub kbuild. Args look like: -j4 olddefconfig [LLVM=1]
set -e
target=""
for arg in "$@"; do
  case "$arg" in
    -j*|LLVM=1) ;;
    *) target="$arg" ;;
  esac
done

case "$target" in
  defconfig)
    cat > .config <<'CFG'
CONFIG_PREEMPT_NONE=y
# CONFIG_PREEMPT is not set
CONFIG_HZ_250=y
CONFIG_HZ=250
CONFIG_MODULES=y
CONFIG_DRM=y
CONFIG_DRM_I915=m
CONFIG_FORCED_OFF=y
CFG
    echo "  GEN     .config"
    ;;
  localmodconfig)
    echo "  using config: '.config'"
    sed -i 's/^CONFIG_DRM_I915=m/# CONFIG_DRM_I915 is not set/' .config
    ;;
  olddefconfig)
    # Kconfig would never keep a symbol the kernel does not define...
    sed -i '/^CONFIG_NOT_IN_THIS_KERNEL=/d' .config
    # ...and some symbols are forced by dependencies, whatever you asked for.
    sed -i 's/^CONFIG_FORCED_OFF=y/# CONFIG_FORCED_OFF is not set/' .config
    echo "#"
    echo "# configuration written to .config"
    ;;
  clean)
    rm -rf arch vmlinux System.map
    echo "  CLEAN   ."
    ;;
  all)
    if [ -f "$(dirname "$0")/slow" ]; then
      echo "  CC      slow.o"
      sleep 120
    fi
    i=1
    while [ "$i" -le "${KT_FAKE_OBJECTS:-12}" ]; do
      echo "  CC      kernel/sched/obj$i.o"
      i=$((i + 1))
    done
    echo "  LD      vmlinux"
    echo "  MODPOST Module.symvers"
    echo "  OBJCOPY arch/x86_64/boot/bzImage"
    mkdir -p arch/x86_64/boot
    echo "fake-kernel-image" > arch/x86_64/boot/bzImage
    echo "fake-system-map" > System.map
    echo "fake-vmlinux" > vmlinux
    ;;
  bindeb-pkg)
    echo "  CC      kernel/main.o"
    echo "  LD      vmlinux"
    echo "  DPKG-DEB building package"
    mkdir -p arch/x86_64/boot
    echo img > arch/x86_64/boot/bzImage
    echo deb > ../linux-image-6.12.0-kertweaker_amd64.deb
    echo deb > ../linux-headers-6.12.0-kertweaker_amd64.deb
    ;;
  modules_install)
    echo "  INSTALL /lib/modules"
    ;;
  install)
    echo "  INSTALL /boot"
    ;;
  *)
    echo "stub make: no rule to make target '$target'" >&2
    exit 2
    ;;
esac
"##;

/// A directory that looks enough like a kernel tree for `is_kernel_tree`.
pub fn fixture(version: &str) -> Fixture {
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("kertweaker-it-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let tree = root.join("sources").join(format!("linux-{version}"));
    let workdir = root.join("work");
    fs::create_dir_all(tree.join("kernel")).unwrap();
    fs::create_dir_all(&workdir).unwrap();

    let (major, rest) = version.split_once('.').unwrap();
    let (patch, sub) = rest.split_once('.').unwrap_or((rest, "0"));
    fs::write(
        tree.join("Makefile"),
        format!(
            "VERSION = {major}\nPATCHLEVEL = {patch}\nSUBLEVEL = {sub}\nEXTRAVERSION =\nNAME = Stub\n"
        ),
    )
    .unwrap();
    fs::write(tree.join("Kbuild"), "# stub\n").unwrap();

    let make = root.join("fake-make");
    fs::write(&make, FAKE_MAKE).unwrap();
    set_executable(&make);

    Fixture { root, tree, workdir, make }
}

fn set_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(p, perms).unwrap();
}

impl Fixture {
    /// Make the compile stage sleep, so cancellation can be tested. Scoped to
    /// this fixture's own stub, because an env var would leak into every other
    /// test running in parallel.
    pub fn make_compile_slow(&self) {
        fs::write(self.root.join("slow"), "1").unwrap();
    }
}

pub fn selections(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
