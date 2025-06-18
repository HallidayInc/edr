function resolveLib(): URL {
  const arch = Deno.build.arch;
  const os = Deno.build.os;
  let target = `${arch}-unknown-${os}`;
  if (os === "darwin") {
    target = `${arch}-apple-darwin`;
  }
  const ext = os === "windows" ? "dll" : os === "darwin" ? "dylib" : "so";
  const bundled = new URL(`./edr_deno.${target}.${ext}`, import.meta.url);
  try {
    Deno.statSync(bundled);
    return bundled;
  } catch {
    return new URL(`../../../target/debug/libedr_deno.${ext}`, import.meta.url);
  }
}

const dylib = Deno.dlopen(resolveLib(), {
  context_new: { parameters: [], result: 'u32' },
  context_drop: { parameters: ['u32'], result: 'void' },
  version: { parameters: [], result: 'pointer' },
  provider_new: { parameters: ['u32', 'pointer', 'usize', 'pointer', 'pointer', 'u8'], result: 'u32' },
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

export function provider_new(
  ctx: number,
  config: string,
  logCb: Deno.PointerValue | null,
  decodeCb: Deno.PointerValue | null,
  enable: number,
): number {
  const data = encode(config);
  const ptr = Deno.UnsafePointer.of(data);
  const cb = logCb ?? null;
  const dec = decodeCb ?? null;
  return dylib.symbols.provider_new(ctx, ptr, BigInt(data.length), cb, dec, enable);
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
  createProvider(
    config: Record<string, unknown>,
    logger?: {
      enable?: boolean;
      printLineCallback?: (msg: string, replace: boolean) => void;
      decodeConsoleLogInputsCallback?: (data: Uint8Array) => void;
    },
  ): Provider {
    let callbackPtr: Deno.PointerValue | null = null;
    let callback: Deno.UnsafeCallback | null = null;
    let decodePtr: Deno.PointerValue | null = null;
    let decodeCb: Deno.UnsafeCallback | null = null;
    const enabled = logger?.enable !== false;
    if (logger?.printLineCallback) {
      callback = new Deno.UnsafeCallback({
        parameters: ['pointer', 'usize', 'u8'] as const,
        result: 'void' as const,
      }, (ptr: Deno.PointerValue, len: bigint, replace: number) => {
        const view = new Deno.UnsafePointerView(ptr!);
        const bytes = new Uint8Array(view.getArrayBuffer(Number(len)));
        const msg = new TextDecoder().decode(bytes);
        logger.printLineCallback!(msg, !!replace);
      }) as unknown as Deno.UnsafeCallback;
      callbackPtr = callback!.pointer;
    }
    if (logger?.decodeConsoleLogInputsCallback) {
      decodeCb = new Deno.UnsafeCallback({
        parameters: ['pointer', 'usize'] as const,
        result: 'void' as const,
      }, (ptr: Deno.PointerValue, len: bigint) => {
        const view = new Deno.UnsafePointerView(ptr!);
        const bytes = new Uint8Array(view.getArrayBuffer(Number(len)));
        logger.decodeConsoleLogInputsCallback!(bytes);
      }) as unknown as Deno.UnsafeCallback;
      decodePtr = decodeCb.pointer;
    }
    const id = provider_new(
      this.#id,
      JSON.stringify(config),
      callbackPtr,
      decodePtr,
      enabled ? 1 : 0,
    );
    return new Provider(id, callback, decodeCb);
  }
  close() {
    ctxFinalizer.unregister(this);
    context_drop(this.#id);
  }
  [Symbol.dispose]() {
    this.close();
  }
  async [Symbol.asyncDispose]() {
    this.close();
  }
}

export class Provider {
  #id: number;
  #callback: Deno.UnsafeCallback | null;
  #decode: Deno.UnsafeCallback | null;
  constructor(id: number, cb: Deno.UnsafeCallback | null, dec: Deno.UnsafeCallback | null) {
    this.#id = id;
    this.#callback = cb;
    this.#decode = dec;
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
    if (this.#callback) this.#callback.close();
    if (this.#decode) this.#decode.close();
  }
  [Symbol.dispose]() {
    this.close();
  }
  async [Symbol.asyncDispose]() {
    this.close();
  }
}
