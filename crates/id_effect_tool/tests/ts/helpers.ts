// Test helpers for the generated client (no dependencies: Deno's test runner only).
// `deno test … -- <url>` passes the test server's RPC URL; the server tests are skipped
// without it (tests/rpc_gen_ts.rs always passes one).

import type { FetchLike, WebSocketLike } from "../golden/rpc/shop/client.ts";

/** The test server's RPC endpoint (`http://127.0.0.1:PORT/rpc`), when given. */
export const url: string | undefined = Deno.args[0];

/** The server URL; only called by server tests. */
export function server(): string {
	if (url === undefined) throw new Error("no server URL");
	return url;
}

/** A test against the server (ignored without a URL). */
export function serverTest(name: string, fn: () => Promise<void>): void {
	Deno.test({
		name,
		ignore: url === undefined,
		sanitizeOps: false,
		sanitizeResources: false,
		fn,
	});
}

/** A test on scripted sockets (their timers outlive the test by a tick). */
export function fakeTest(name: string, fn: () => Promise<void>): void {
	Deno.test({ name, sanitizeOps: false, fn });
}

export function assert(
	cond: unknown,
	message = "assertion failed",
): asserts cond {
	if (!cond) throw new Error(message);
}

function equal(a: unknown, b: unknown): boolean {
	if (a === b) return true;
	if (
		typeof a !== "object" ||
		typeof b !== "object" ||
		a === null ||
		b === null
	) {
		return false;
	}
	if (Array.isArray(a) !== Array.isArray(b)) return false;
	const ka = Object.keys(a).filter(
		(k) => (a as Record<string, unknown>)[k] !== undefined,
	);
	const kb = Object.keys(b).filter(
		(k) => (b as Record<string, unknown>)[k] !== undefined,
	);
	if (ka.length !== kb.length) return false;
	return ka.every((k) =>
		equal((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]),
	);
}

export function assertEquals(
	actual: unknown,
	expected: unknown,
	message = "not equal",
): void {
	if (!equal(actual, expected)) {
		throw new Error(
			`${message}\n  actual:   ${JSON.stringify(actual)}\n  expected: ${JSON.stringify(expected)}`,
		);
	}
}

/** `promise` rejects with an error `check` accepts. */
export async function assertRejects(
	promise: Promise<unknown>,
	check: (error: unknown) => boolean,
	message = "did not reject as expected",
): Promise<unknown> {
	try {
		await promise;
	} catch (error) {
		if (!check(error)) throw new Error(`${message}: ${String(error)}`);
		return error;
	}
	throw new Error(`${message}: it resolved`);
}

export function isAbort(error: unknown): boolean {
	return error instanceof DOMException && error.name === "AbortError";
}

/** Every item of `stream`. */
export async function collect<T>(stream: AsyncIterable<T>): Promise<T[]> {
	const out: T[] = [];
	for await (const item of stream) out.push(item);
	return out;
}

export interface Recorded {
	readonly url: string;
	readonly method: string;
	readonly headers: Record<string, string>;
	readonly body: string | undefined;
}

/** `fetch` that records every request, then answers with `answer` (default: the real fetch). */
export function recordingFetch(
	answer?: (request: Recorded) => Response | Promise<Response>,
): {
	fetch: FetchLike;
	requests: Recorded[];
} {
	const requests: Recorded[] = [];
	const fetch: FetchLike = (input, init) => {
		const headers: Record<string, string> = {};
		new Headers(init.headers).forEach((value, name) => {
			headers[name] = value;
		});
		const request: Recorded = {
			url: input,
			method: init.method ?? "GET",
			headers,
			body: typeof init.body === "string" ? init.body : undefined,
		};
		requests.push(request);
		return answer
			? Promise.resolve(answer(request))
			: globalThis.fetch(input, init);
	};
	return { fetch, requests };
}

/** A JSON response. */
export function json(
	body: unknown,
	status = 200,
	type = "application/json",
): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { "content-type": type },
	});
}

/** Answers a batch (or a single call) with each call's id echoed and its payload as the value. */
export function echo(request: Recorded): Response {
	const body = JSON.parse(request.body ?? "null");
	const result = (c: { id: number; tag: string; payload: unknown }) => ({
		id: c.id,
		tag: c.tag,
		exit: { _tag: "Success", value: c.payload },
	});
	return json(Array.isArray(body) ? body.map(result) : result(body));
}

export const SESSION = "AAAAAAAAAAAAAAAAAAAAAA";

/** A scripted WebSocket: opens, says hello, records what the client sends. */
export class FakeSocket implements WebSocketLike {
	protocol = "idrpc.json.v1";
	readyState = 1;
	readonly sent: Record<string, unknown>[] = [];
	closed = false;
	private readonly listeners = new Map<string, ((event: unknown) => void)[]>();

	constructor(
		private readonly onSend?: (
			frame: Record<string, unknown>,
			socket: FakeSocket,
		) => void,
		session = SESSION,
		protocol = "idrpc.json.v1",
	) {
		this.protocol = protocol;
		// Microtasks, not timers: nothing outlives a test.
		queueMicrotask(() => {
			this.emit("open", {});
			queueMicrotask(() => this.hello(session));
		});
	}

	addEventListener(type: string, listener: (event: unknown) => void): void {
		const list = this.listeners.get(type) ?? [];
		list.push(listener);
		this.listeners.set(type, list);
	}

	private emit(type: string, event: unknown): void {
		for (const listener of this.listeners.get(type) ?? []) listener(event);
	}

	send(data: string): void {
		const frame = JSON.parse(data) as Record<string, unknown>;
		this.sent.push(frame);
		this.onSend?.(frame, this);
	}

	close(): void {
		if (this.closed) return;
		this.closed = true;
		queueMicrotask(() => this.emit("close", {}));
	}

	/** The server sends `frame` (an object, or raw text). */
	receive(frame: unknown): void {
		if (this.closed) return;
		this.emit("message", {
			data: typeof frame === "string" ? frame : JSON.stringify(frame),
		});
	}

	hello(session = SESSION, window = 32): void {
		this.receive({
			id: 0,
			kind: "hello",
			flags: 0,
			payload: { session, codec: "idrpc.json.v1", max_frame: 65536, window },
		});
	}
}

export function delay(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}
