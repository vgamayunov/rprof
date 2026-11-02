import { build } from 'esbuild';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { checkProjectionWasm } from './check-projection-wasm.mjs';

const check = process.argv.includes('--check');
if (check) await checkProjectionWasm();
const outputs = [
  { entryPoints: ['src/main.ts'], outfile: 'dist/viewer.js', bundle: true, minify: true, format: 'iife', target: 'es2022', legalComments: 'none' },
  { entryPoints: ['src/viewer.css'], outfile: 'dist/viewer.css', bundle: true, minify: true, legalComments: 'none' },
];

for (const options of outputs) {
  const result = await build({ ...options, write: false, logLevel: 'silent' });
  const generated = result.outputFiles[0].contents;
  if (check) {
    let existing;
    try { existing = await readFile(options.outfile); } catch { existing = null; }
    if (!existing || !existing.equals(generated)) {
      console.error(`${options.outfile} is stale; run npm run build`);
      process.exitCode = 1;
    }
  } else {
    await mkdir('dist', { recursive: true });
    await writeFile(options.outfile, generated);
  }
}
