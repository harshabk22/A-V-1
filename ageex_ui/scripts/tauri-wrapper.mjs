import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");
const tauriBin = path.join(
  repoRoot,
  "node_modules",
  ".bin",
  process.platform === "win32" ? "tauri.cmd" : "tauri",
);

const rawArgs = process.argv.slice(2);
const [command, ...rest] = rawArgs;
const args = [...rawArgs];

const child = spawn(tauriBin, args, {
  cwd: repoRoot,
  stdio: "inherit",
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 0);
});

