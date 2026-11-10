# kertweaker for gurt

Files that let gurt install kertweaker.

- [`install.sh`](install.sh) — the script gurt runs. Clones the repository,
  builds the release binary, installs it to `/usr/local/bin/kertweaker`. No
  services, no shell-file edits, no network traffic after the clone.
- [`kertweaker.gurt`](kertweaker.gurt) — a text manifest for a gurt client.

## Install

```bash
gurt install kertweaker
```

Or without gurt:

```bash
curl -fsSL https://raw.githubusercontent.com/clrealy/kertweaker/main/packaging/gurt/install.sh -o install-kertweaker.sh
```

```bash
sh install-kertweaker.sh
```

## Uninstall

```bash
sudo rm /usr/local/bin/kertweaker
```

```bash
rm -rf ~/.local/share/kertweaker/src
```

## Overrides

Environment variables honoured by `install.sh`:

| Variable | Default | What it does |
| --- | --- | --- |
| `PREFIX` | `/usr/local` | Install root. Binary lands at `$PREFIX/bin/kertweaker`. |
| `KERTWEAKER_SRC` | `$XDG_DATA_HOME/kertweaker/src` | Where the source clone lives. |
| `KERTWEAKER_REPO` | `https://github.com/clrealy/kertweaker.git` | Repository to clone. |
| `KERTWEAKER_REF` | `main` | Branch or tag to check out. |
