function resolveLib(): URL {
    const target = Deno.build.target;
    const ext = Deno.build.os === "darwin" ? "dylib" : "so";
    return new URL(`./edr_deno.${target}.${ext}`, import.meta.url);
}

const dylib = Deno.dlopen(resolveLib(), {
    context_new: { parameters: [], result: "u32" },
    context_drop: { parameters: ["u32"], result: "void" },
    version: { parameters: [], result: "pointer" },
    provider_new: {
        parameters: ["u32", "pointer", "usize", "pointer", "pointer", "u8"],
        result: "u32",
    },
    provider_handle_request: {
        parameters: ["u32", "pointer", "usize"],
        result: "pointer",
    },
    provider_set_verbose_tracing: { parameters: ["u32", "u8"], result: "void" },
    provider_drop: { parameters: ["u32"], result: "void" },
});

function encode(str: string): Uint8Array {
    return new TextEncoder().encode(str);
}

function decode(ptr: Deno.PointerValue): string {
    if (ptr === null) return "";
    const view = new Deno.UnsafePointerView(ptr);
    const len = (view.getUint8(0) << 24) | (view.getUint8(1) << 16) |
        (view.getUint8(2) << 8) | view.getUint8(3);
    const all = new Uint8Array(view.getArrayBuffer(4 + len));
    const text = new TextDecoder().decode(all.subarray(4));
    return text;
}

type LoggerEntry = {
    printLineCallback?: (msg: string, replace: boolean) => void;
    decodeConsoleLogInputsCallback?: (inputs: Uint8Array[]) => void;
};

const loggerMap = new Map<number, LoggerEntry>();

const globalLogCb = new Deno.UnsafeCallback({
    parameters: ["u32", "pointer", "usize", "u8"] as const,
    result: "void" as const,
}, (id: number, ptr: Deno.PointerValue, len: bigint, replace: number) => {
    const entry = loggerMap.get(id);
    if (!entry?.printLineCallback) return;
    const view = new Deno.UnsafePointerView(ptr!);
    const bytes = new Uint8Array(view.getArrayBuffer(Number(len)));
    const msg = new TextDecoder().decode(bytes);
    entry.printLineCallback(msg, !!replace);
}) as unknown as Deno.UnsafeCallback;

const globalDecodeCb = new Deno.UnsafeCallback({
    parameters: ["u32", "pointer", "usize"] as const,
    result: "void" as const,
}, (id: number, ptr: Deno.PointerValue, len: bigint) => {
    const entry = loggerMap.get(id);
    if (!entry?.decodeConsoleLogInputsCallback) return;
    const view = new Deno.UnsafePointerView(ptr!);
    const bytes = new Uint8Array(view.getArrayBuffer(Number(len)));
    const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    let offset = 0;
    const count = dv.getUint32(offset, true);
    offset += 4;
    const inputs: Uint8Array[] = [];
    for (let i = 0; i < count; i++) {
        const l = dv.getUint32(offset, true);
        offset += 4;
        inputs.push(bytes.slice(offset, offset + l));
        offset += l;
    }
    entry.decodeConsoleLogInputsCallback(inputs);
}) as unknown as Deno.UnsafeCallback;

function stringify(value: unknown): string {
    return JSON.stringify(value, (_k, v) => {
        if (typeof v === "bigint") {
            return v.toString();
        }
        return v;
    });
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
    return dylib.symbols.provider_new(
        ctx,
        ptr,
        BigInt(data.length),
        cb,
        dec,
        enable,
    );
}

export async function provider_handle_request(
    id: number,
    req: string,
): Promise<string> {
    const data = encode(req);
    const ptrIn = Deno.UnsafePointer.of(data);
    const ptr = dylib.symbols.provider_handle_request(
        id,
        ptrIn,
        BigInt(data.length),
    );
    return decode(ptr);
}

export function provider_set_verbose_tracing(
    id: number,
    enabled: number,
): void {
    dylib.symbols.provider_set_verbose_tracing(id, enabled);
}

export function provider_drop(id: number): void {
    dylib.symbols.provider_drop(id);
}

export class Context {
    #id: number;

    constructor() {
        this.#id = context_new();
    }

    createProvider(
        config: Record<string, unknown>,
        logger?: {
            enable?: boolean;
            printLineCallback?: (msg: string, replace: boolean) => void;
            decodeConsoleLogInputsCallback?: (inputs: Uint8Array[]) => void;
        },
    ): Provider {
        const json = stringify(config);
        const enabled = logger?.enable !== false;
        const id = provider_new(
            this.#id,
            json,
            globalLogCb.pointer,
            globalDecodeCb.pointer,
            enabled ? 1 : 0,
        );
        loggerMap.set(id, {
            printLineCallback: logger?.printLineCallback,
            decodeConsoleLogInputsCallback: logger?.decodeConsoleLogInputsCallback,
        });
        return new Provider(id);
    }

    close() {
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

    constructor(id: number) {
        this.#id = id;
    }

    async handleRequest(req: string) {
        return { data: await provider_handle_request(this.#id, req) };
    }

    setVerboseTracing(enabled: boolean) {
        provider_set_verbose_tracing(this.#id, enabled ? 1 : 0);
    }

    close() {
        provider_drop(this.#id);
        loggerMap.delete(this.#id);
    }

    [Symbol.dispose]() {
        this.close();
    }

    async [Symbol.asyncDispose]() {
        this.close();
    }
}
