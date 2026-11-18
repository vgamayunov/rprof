import { aggregateStacks } from './stacks.js';
import type { StackData, StackQuery } from './stacks.js';
import type { Track } from './model.js';

let data: StackData | undefined;
let tracks: Track[] = [];
type Request = { kind: 'init'; data: StackData; tracks: Track[] } | { kind: 'query'; id: number; query: StackQuery };
self.onmessage = (event: MessageEvent<Request>) => {
  const request = event.data;
  if (request.kind === 'init') { data = request.data; tracks = request.tracks; return; }
  try {
    if (!data) throw new Error('Function worker has no capture data');
    self.postMessage({ id: request.id, tree: aggregateStacks(data, tracks, request.query) });
  } catch (error) {
    self.postMessage({ id: request.id, error: error instanceof Error ? error.message : String(error) });
  }
};
