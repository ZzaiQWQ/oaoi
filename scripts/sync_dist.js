const fs = require('fs');
const path = require('path');

const rootDir = path.resolve(__dirname, '..');
const distDir = path.join(rootDir, 'dist');

const entries = [
  ['index.html', 'index.html'],
  ['js', 'js'],
  ['styles', 'styles'],
  ['assets', 'assets'],
];

function copyEntry(sourceRel, targetRel) {
  const source = path.join(rootDir, sourceRel);
  const target = path.join(distDir, targetRel);

  if (!fs.existsSync(source)) {
    throw new Error(`Missing frontend entry: ${sourceRel}`);
  }

  fs.cpSync(source, target, {
    recursive: true,
    force: true,
    errorOnExist: false,
  });
}

function syncFrontend() {
  fs.rmSync(distDir, { recursive: true, force: true });
  fs.mkdirSync(distDir, { recursive: true });

  for (const [sourceRel, targetRel] of entries) {
    copyEntry(sourceRel, targetRel);
  }
}

function syncReleaseExtras() {
  const modcnSource = path.join(rootDir, 'src-tauri', 'modcn.txt');
  if (!fs.existsSync(modcnSource)) return;

  const releaseDir = path.join(rootDir, 'src-tauri', 'target', 'release');
  fs.mkdirSync(releaseDir, { recursive: true });
  fs.copyFileSync(modcnSource, path.join(releaseDir, 'modcn.txt'));
}

syncFrontend();

if (process.argv.includes('--release')) {
  syncReleaseExtras();
}

console.log(`[sync-dist] frontend copied to ${path.relative(rootDir, distDir)}`);
