## 🧬 Plexus

**A high-performance, terminal-based task runner and process manager built for speed.**

Plexus is a developer-centric tool designed to orchestrate complex task workflows with a modern Terminal User Interface (TUI). Whether you're managing microservices, long-running build pipelines, or complex monorepos, Plexus provides the observability and control you need directly in your terminal.

---

## ✨ Features

- **Native Performance:** Built with **Rust** for near-zero overhead and memory safety.
- **Modern TUI:** Interactive interface powered by `ratatui` for real-time process monitoring.
- **Cross-Platform:** Native binaries distributed via npm for Linux (x64/ARM), macOS, and Windows.
- **Developer-First:** Designed to work seamlessly in `pnpm` monorepos and advanced development environments.

---

## 🚀 Quick Start

You can run Plexus immediately without a permanent installation using `npx`:

```bash
npx @m.hesari/plexus
```

### Installation

Install it globally via your preferred package manager:

```bash
# Using pnpm (Recommended)
pnpm add -g @m.hesari/plexus

# Using npm
npm install -g @m.hesari/plexus
```

---

## 🛠 Configuration

Plexus looks for a configuration file in your project root (e.g., `plexus.json`).

```json
{
  "tasks": {
    "dev": "pnpm run dev",
    "build": "cargo build --release",
    "test": "npm test"
  }
}
```

---

## 🖱 Mouse scrolling in the TUI

Mouse capture is off by default, so your terminal keeps its own click-drag text selection.
Start with `--mouse`, or press `m` while the TUI is running, to let the wheel scroll the log.
The status bar shows `Wheel on` or `Wheel off` so you can tell which mode you are in.

Inside tmux you need both halves, because tmux does not forward mouse events to a pane
unless you have asked it to:

```tmux
set -g mouse on
```

Without that, tmux never asks the terminal for mouse events and nothing reaches Plexus, no
matter which flags you pass. `j`/`k`, `PgUp`/`PgDn` and `Home`/`End` always work regardless.

---

## 🤖 AI agents

While `plexus run -W` is running, other processes can drive it with `plexus client`: list
packages, run a type check and get its exit code, restart a dev server, read output, find and
forward ports. That lets an AI agent use your running session instead of starting its own
copies of every script.

Plexus ships the instructions for this inside the binary, so they always match the version
you have installed. They aren't tied to any particular agent:

```bash
plexus skills list                  # what's available
plexus skills get                   # how to use them, including the AGENTS.md line below
plexus skills get client            # the main skill, as a SKILL.md with frontmatter
plexus skills get client/forwarding # a reference doc the skill links to
```

Your agent can fetch them at runtime, or you can save them wherever your agent keeps skills:

```bash
plexus skills get client > <skills dir>/plexus-client/SKILL.md
```

The skill links to its reference docs by command, so a saved copy works the same as fetching it
each time.

### Telling the agent about it

An agent only reads a skill it knows about. If you don't save the skill, add this line to
`AGENTS.md`, `CLAUDE.md`, or whatever file your agent reads at the start of a session:

```
A plexus watch session may be running in this repo; before running builds, type checks, lints or dev servers yourself, run `plexus skills get client` and follow it.
```

If `plexus` isn't on PATH (installed per project), use the command you run it with, in that
line and in the skill text:

```bash
plexus skills get client --cmd "pnpm exec plexus"
```

---

## 📦 Architecture

Plexus uses a **hybrid distribution model**:

1.  **Core:** High-performance Rust binaries tailored for specific CPU architectures.
2.  **Wrapper:** A lightweight Node.js loader that detects your system profile (OS/Arch) and executes the correct native binary.

---

## 🏗 Development

If you want to build Plexus from source:

1.  **Clone the repo:**
    ```bash
    git clone [https://github.com/mohamad-hesari/plexus.git](https://github.com/mohamad-hesari/plexus.git)
    ```
2.  **Build the Rust core:**
    ```bash
    cargo build --release
    ```
3.  **Run the binary:**
    ```bash
    ./target/release/plexus
    ```

---

## 📄 License

Distributed under the MIT License. See `LICENSE` for more information.
