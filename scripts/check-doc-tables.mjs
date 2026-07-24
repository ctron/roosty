import { readFileSync } from 'node:fs'

const pages = [
  'build/site/development/compatibility.html',
  'build/site/development/development.html',
]

for (const page of pages) {
  const html = readFileSync(page, 'utf8')
  const tables = [...html.matchAll(/<table\b[\s\S]*?<\/table>/g)]

  if (tables.length === 0) {
    throw new Error(`${page} contains no tables`)
  }

  for (const [index, table] of tables.entries()) {
    const header = table[0].match(/<thead>[\s\S]*?<\/thead>/)?.[0] ?? ''
    const columns = [...header.matchAll(/<th\b/g)].length

    if (columns !== 3) {
      throw new Error(`${page} table ${index + 1} has ${columns} header columns; expected 3`)
    }
  }
}
