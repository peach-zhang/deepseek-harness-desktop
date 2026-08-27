import { readFile, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const harnessVersion = (await readFile(join(projectRoot, 'HARNESS_VERSION'), 'utf8')).trim()

if (!harnessVersion) {
  throw new Error('HARNESS_VERSION must not be empty')
}

const output = `// Auto-generated from HARNESS_VERSION - do not edit.\nexport const HARNESS_VERSION = ${JSON.stringify(harnessVersion)};\n`
await writeFile(join(projectRoot, 'src', 'generated-version.ts'), output)
