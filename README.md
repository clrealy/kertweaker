# kertweaker

Tweak a Linux kernel from a GUI, then build it for the machine you are sitting at.

You pick things in plain terms - "full preemption", "BBR networking", "no debug
info" - and kertweaker shows exactly which `CONFIG_` options that changes,
builds the kernel, and tells you about anything kbuild refused to apply.

- **One small binary, no dependencies.** Pure Rust standard library. The GUI is
  served from the binary to your browser on `127.0.0.1`.
- **Honest about what happened.** After `make olddefconfig`, every option you
  asked for is checked against the final config. Anything that did not stick is
  listed, instead of being discovered after a two-hour build.
- **Never installs behind your back.** It builds, then hands you the exact
  commands to install. Installing only happens if you ask *and* run it as root.

## Quick start

```bash
cargo build --release
```

```bash
./target/release/kertweaker
```

It prints a link such as `http://127.0.0.1:7878/?token=...`. Open it (it opens
by itself when there is a desktop session), then:

1. **Start** - check your machine is ready, pick a profile, choose the kernel
   source and the config to start from.
2. **Tweaks** - adjust anything. Each option shows the config lines it sets.
3. **Review & build** - see the before/after diff, then build.

Check a machine before you start:

```bash
./target/release/kertweaker doctor
```

It lists every build tool and prints the one command that installs what is
missing on Debian/Ubuntu, Fedora/RHEL or Arch.

## What you can tweak

42 tweaks in ten groups, plus an "extra options" box for raw config lines:

| Group | Examples |
| --- | --- |
| Latency & scheduling | preemption model, timer frequency, tickless, autogroup |
| CPU & power | default frequency governor, CPU family, suspend/hibernate |
| Memory | transparent huge pages, MGLRU, zswap, zram, KSM |
| Storage & filesystems | I/O schedulers, filesystem sets, NFS/SMB |
| Networking | BBR, fq_codel/CAKE, WireGuard |
| Graphics & hardware | which GPU drivers to build, sound |
| Virtualisation & containers | KVM, virtio guest, container support, VFIO |
| Security & hardening | hardening levels, LSM, module signing, mitigations |
| Build size & speed | debug info, localmodconfig, LTO, compression, modules |
| Naming | version suffix |

Seven built-in profiles: `desktop`, `gaming`, `laptop`, `server`, `vm_guest`,
`fast_build` and `hardened`. The GUI suggests one for your machine. You can
save your own tweak sets and reapply them when the next kernel comes out.

Options are marked **Caution** or **Risky** where they can cost you a bootable
or secure machine, and risky selections need an explicit confirmation.

## How it decides what your config ends up as

kertweaker does not reimplement Kconfig's dependency solver. It:

1. Starts from a base config - by default the one your running kernel uses, so
   every driver your hardware needs is already on.
2. Optionally trims to the modules you have loaded (`make localmodconfig`).
3. Applies your tweaks the way the kernel's own `merge_config.sh` does.
4. Runs `make olddefconfig` so kbuild resolves dependencies.
5. Compares the result with what you asked for and reports the differences:
   options that did not take effect, options built in rather than as modules,
   and why when it can tell (for example, module support being switched off).

## Command line

Everything the GUI does is scriptable:

```bash
kertweaker profiles
```

```bash
kertweaker tweaks --group latency
```

```bash
kertweaker fragment --profile desktop --set hz=300
```

```bash
kertweaker build --kernel 6.12.1 --profile desktop --output deb
```

```bash
kertweaker build --tree ~/src/linux --base defconfig --set preempt=full --config-only
```

`kertweaker --help` lists every flag. `--config-only` writes and verifies the
`.config` without compiling, which is a quick way to try a selection.

## Requirements

- Linux, and Rust 1.74 or newer to build kertweaker itself.
- To build kernels: `gcc`, `make`, `flex`, `bison`, `bc`, `openssl`, `tar`,
  `xz`, `gzip`, `kmod`, plus the libelf and ncurses headers. `kertweaker
  doctor` prints the exact install command for your distribution.
- `curl` or `wget` to download kernels from kernel.org. Not needed if you point
  it at a source tree you already have.
- Optional, depending on what you select: `pahole` (BTF), `clang` + `lld`
  (LTO), `zstd`/`lz4` (compression), GCC plugin headers (stack-leak clearing
  and struct randomisation on kernels before 6.17).

Disk and time: a full build uses 2-30 GiB depending mostly on debug info. The
*Fastest possible build* profile is the quickest way to a first kernel.

## Installing what you built

kertweaker shows these for you with a copy button; they are listed here so you
know what to expect.

Debian packages (recommended on Debian/Ubuntu - it removes cleanly too):

```bash
sudo dpkg -i linux-image-*.deb linux-headers-*.deb
```

Plain image, run from the source tree:

```bash
sudo make modules_install
```

```bash
sudo make install
```

Keep your current kernel installed. If the new one misbehaves, pick the old one
from the boot menu.

**Secure Boot:** a kernel you build yourself is unsigned. With Secure Boot on,
the firmware refuses it unless you sign it with an enrolled key (MOK) or turn
Secure Boot off. `kertweaker doctor` and the GUI both warn when it is on.

## Security

The GUI can start builds, so access to it is restricted:

- It listens on `127.0.0.1` only.
- Every run generates a random access token. The printed link carries it once;
  it is exchanged for an `HttpOnly`, `SameSite=Strict` cookie and never
  embedded in a page.
- The `Host` header is checked, which defeats DNS rebinding, and cross-origin
  POSTs are refused.
- Kernel versions and saved-set names are validated before they reach a
  command line or a file name. Commands are run without a shell.

Run it as your normal user. Only the final install step needs root, and you run
that yourself.

## Where things are kept

`~/.local/share/kertweaker` by default (`--workdir` or `KERTWEAKER_HOME` to
change it): downloaded tarballs, extracted sources, the config and fragment
from every build, full build logs, and your saved tweak sets.

## Development

```bash
cargo test
```

The suite includes an end-to-end run of the whole build pipeline against a stub
kbuild, so it needs no kernel tree and finishes in about a second.

The tweak catalog lives in `src/catalog.rs`. Tests check that every profile
resolves, that no two tweaks fight over a symbol, and that renamed symbols are
declared as aliases. Before adding an option, confirm it against a real tree:

```bash
kertweaker build --tree /path/to/linux --base defconfig --set TWEAK=OPTION --config-only
```

If verification reports the option did not stick, it is missing a dependency.

## License

MIT
