const libPath = new URL('../../../target/debug/libedr_deno.so', import.meta.url);
const dylib = Deno.dlopen(libPath, {
  context_new: { parameters: [], result: 'u32' },
  context_drop: { parameters: ['u32'], result: 'void' },
  version: { parameters: [], result: 'pointer' },
  provider_new: { parameters: ['u32', 'pointer', 'usize'], result: 'u32' },
  provider_handle_request: { parameters: ['u32', 'pointer', 'usize'], result: 'pointer' },
  provider_set_verbose_tracing: { parameters: ['u32', 'u8'], result: 'void' },
  provider_drop: { parameters: ['u32'], result: 'void' },
});

function encode(str: string): Uint8Array {
  return new TextEncoder().encode(str);
}

function decode(ptr: Deno.PointerValue): string {
  if (ptr === null) return "";
  const view = new Deno.UnsafePointerView(ptr);
  const len =
    (view.getUint8(0) << 24) | (view.getUint8(1) << 16) | (view.getUint8(2) << 8) | view.getUint8(3);
  const all = new Uint8Array(view.getArrayBuffer(4 + len));
  const text = new TextDecoder().decode(all.subarray(4));
  return text;
}

export function context_new(): number {
  return dylib.symbols.context_new();
}

export function context_drop(id: number): void {
  dylib.symbols.context_drop(id);
}

export function version(): string {
  const ptr = dylib.symbols.version();
  return decode(ptr);
}

export function provider_new(ctx: number, config: string): number {
  const data = encode(config);
  const ptr = Deno.UnsafePointer.of(data);
  return dylib.symbols.provider_new(ctx, ptr, BigInt(data.length));
}

export async function provider_handle_request(id: number, req: string): Promise<string> {
  const data = encode(req);
  const ptrIn = Deno.UnsafePointer.of(data);
  const ptr = dylib.symbols.provider_handle_request(id, ptrIn, BigInt(data.length));
  return decode(ptr);
}

export function provider_set_verbose_tracing(id: number, enabled: number): void {
  dylib.symbols.provider_set_verbose_tracing(id, enabled);
}

export function provider_drop(id: number): void {
  dylib.symbols.provider_drop(id);
}

const ctxFinalizer = new FinalizationRegistry<number>((id) => {
  dylib.symbols.context_drop(id);
});

const providerFinalizer = new FinalizationRegistry<number>((id) => {
  dylib.symbols.provider_drop(id);
});

export class Context {
  #id: number;
  constructor() {
    this.#id = context_new();
    ctxFinalizer.register(this, this.#id);
  }
  createProvider(config: Record<string, unknown>): Provider {
    const id = provider_new(this.#id, JSON.stringify(config));
    return new Provider(id);
  }
  close() {
    ctxFinalizer.unregister(this);
    context_drop(this.#id);
  }
}

export class Provider {
  #id: number;
  constructor(id: number) {
    this.#id = id;
    providerFinalizer.register(this, this.#id);
  }
  async handleRequest(req: unknown): Promise<unknown> {
    const res = await provider_handle_request(this.#id, JSON.stringify(req));
    return JSON.parse(res);
  }
  setVerboseTracing(enabled: boolean) {
    provider_set_verbose_tracing(this.#id, enabled ? 1 : 0);
  }
  close() {
    providerFinalizer.unregister(this);
    provider_drop(this.#id);
  }
}
