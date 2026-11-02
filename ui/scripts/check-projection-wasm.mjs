import { createHash } from 'node:crypto';
import { readFile, readdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve, dirname } from 'node:path';
import { execFileSync } from 'node:child_process';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'../..');
export async function checkProjectionWasm(write=false) {
  const files=['Cargo.lock','Cargo.toml','crates/rprof-project/Cargo.toml',
    ...(await readdir(resolve(root,'crates/rprof-project/src'))).filter(f=>f.endsWith('.rs')).sort().map(f=>`crates/rprof-project/src/${f}`)];
  const hash=createHash('sha256');
  for(const file of files){hash.update(file);hash.update(await readFile(resolve(root,file)));}
  const sources_sha256=hash.digest('hex');
  const wasm_sha256=createHash('sha256').update(await readFile(resolve(root,'ui/dist/project.wasm'))).digest('hex');
  const path=resolve(root,'ui/dist/project-wasm.json');
  if(write){await writeFile(path,JSON.stringify({sources_sha256,wasm_sha256,rustc:execFileSync('rustc',['--version'],{encoding:'utf8'}).trim()},null,2)+'\n');return;}
  const saved=JSON.parse(await readFile(path,'utf8'));
  if(saved.sources_sha256!==sources_sha256||saved.wasm_sha256!==wasm_sha256)throw new Error('Projection WASM is stale; run bash scripts/build-projection-wasm.sh');
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await checkProjectionWasm(process.argv.includes('--write'));
