// The private tools config for the Node tools, with exactly the rules of tools/tools-env.inc.sh: KEY=VALUE lines in
// $KIMA_RS_TOOLS_ENV (default ${XDG_CONFIG_HOME:-~/.config}/kima-rs/tools.env), values literal, only the six keys
// below, the first line of a key counts, and a key present in the environment (even empty) wins over the config.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const TOOLS_ENV_KEYS = ['KIMA_HUB_REFERENCE', 'KIMA_REFERENCE_COPY', 'KIMA_CSS_DEPS', 'KIMA_TOOLS_PATH', 'VERTIGO', 'FFMPEG'];

export function toolsEnv(key) {
  if (!TOOLS_ENV_KEYS.includes(key)) throw new Error(`${key} is not a tools config key`);
  if (Object.prototype.hasOwnProperty.call(process.env, key)) return process.env[key];
  const file = process.env.KIMA_RS_TOOLS_ENV || path.join(process.env.XDG_CONFIG_HOME || path.join(os.homedir(), '.config'), 'kima-rs/tools.env');
  if (!fs.existsSync(file)) return undefined;
  const line = fs.readFileSync(file, 'utf8').split('\n').find((l) => l.startsWith(`${key}=`));
  return line === undefined ? undefined : line.slice(key.length + 1);
}
