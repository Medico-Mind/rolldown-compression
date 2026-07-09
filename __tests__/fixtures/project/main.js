import { greet } from './lib.js'

const messages = []
for (let index = 0; index < 100; index++) {
  messages.push(greet(`user-${index}`))
}

export function report() {
  return messages.join('\n')
}

export const banner = [
  'lorem ipsum dolor sit amet consectetur adipiscing elit',
  'sed do eiusmod tempor incididunt ut labore et dolore magna aliqua',
  'ut enim ad minim veniam quis nostrud exercitation ullamco laboris',
].join(' | ')
