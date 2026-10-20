// Every server frame is validated before the client acts on it (design note §5), and a server
// that breaks the protocol loses its connection.

import {
	RpcConnection,
	RpcTransportError,
	ShopV1Client,
	backoffDelay,
	decodeServerFrame,
	isCursor,
	isWireExit,
	randomKey,
} from "../golden/rpc/shop/client.ts";
import {
	assert,
	assertEquals,
	assertRejects,
	collect,
	delay,
	FakeSocket,
	fakeTest,
	SESSION,
} from "./helpers.ts";

const hello = {
	session: SESSION,
	codec: "idrpc.json.v1",
	max_frame: 65536,
	window: 32,
};

Deno.test("well-formed server frames decode", () => {
	const good: [unknown, unknown][] = [
		[
			{ id: 1, kind: "item", flags: 0, payload: 3 },
			{ id: 1, kind: "item", flags: 0, payload: 3 },
		],
		[
			{ id: 1, kind: "item" },
			{ id: 1, kind: "item", flags: 0 },
		],
		[
			{ id: 1, kind: "item", cursor: "c1.AgAAAAAAAAAH", payload: "x" },
			{
				id: 1,
				kind: "item",
				flags: 0,
				cursor: "c1.AgAAAAAAAAAH",
				payload: "x",
			},
		],
		[
			{ id: 1, kind: "end", flags: 1 },
			{ id: 1, kind: "end", flags: 1 },
		],
		[
			{ id: 1, kind: "exit", payload: { _tag: "Success", value: null } },
			{
				id: 1,
				kind: "exit",
				flags: 0,
				payload: { _tag: "Success", value: null },
			},
		],
		[
			{
				id: 2,
				kind: "exit",
				payload: { _tag: "Failure", error: { _tag: "Locked" } },
			},
			{
				id: 2,
				kind: "exit",
				flags: 0,
				payload: { _tag: "Failure", error: { _tag: "Locked" } },
			},
		],
		[
			{ id: 1, kind: "credit", credits: 16 },
			{ id: 1, kind: "credit", flags: 0, credits: 16 },
		],
		[
			{ id: 0, kind: "hello", payload: hello },
			{ id: 0, kind: "hello", flags: 0, payload: hello },
		],
		[
			{ id: 0, kind: "ping" },
			{ id: 0, kind: "ping", flags: 0 },
		],
		[
			{ id: 0, kind: "pong", extra: 1 },
			{ id: 0, kind: "pong", flags: 0 },
		],
	];
	for (const [frame, want] of good) {
		assertEquals(
			decodeServerFrame(JSON.stringify(frame)),
			want,
			JSON.stringify(frame),
		);
	}
});

Deno.test("malformed server frames are refused", () => {
	const bad: unknown[] = [
		new Uint8Array([1]),
		"not json",
		"[1]",
		"null",
		{ id: -1, kind: "item" },
		{ id: 1.5, kind: "item" },
		{ id: 2 ** 53, kind: "item" },
		{ id: "1", kind: "item" },
		{ id: 1, kind: "shout" },
		{ id: 1, kind: "call", tag: "A.b" },
		{ id: 1, kind: "cancel" },
		{ id: 0, kind: "resume", payload: {} },
		{ id: 1, kind: "item", flags: 4 },
		{ id: 1, kind: "item", flags: 256 },
		{ id: 1, kind: "item", flags: "0" },
		{ id: 1, kind: "hello", payload: hello },
		{ id: 0, kind: "item" },
		{ id: 1, kind: "item", tag: "A.b" },
		{ id: 1, kind: "item", v: 1 },
		{ id: 1, kind: "item", credits: 1 },
		{
			id: 1,
			kind: "exit",
			cursor: "c1.AgAAAAAAAAAH",
			payload: { _tag: "Success", value: 1 },
		},
		{ id: 1, kind: "item", cursor: "c2.AgAAAAAAAAAH" },
		{ id: 1, kind: "item", cursor: `c1.${"A".repeat(62)}` },
		{ id: 1, kind: "exit" },
		{ id: 1, kind: "exit", payload: { _tag: "Success" } },
		{ id: 1, kind: "exit", payload: { _tag: "Failure", error: "x" } },
		{ id: 1, kind: "exit", payload: { _tag: "Maybe" } },
		{ id: 0, kind: "hello", payload: { ...hello, codec: "idrpc.postcard.v1" } },
		{ id: 0, kind: "hello", payload: { ...hello, session: "short" } },
		{ id: 0, kind: "hello", payload: { ...hello, max_frame: 1 << 20 } },
		{ id: 0, kind: "hello", payload: { ...hello, window: 0 } },
		{ id: 0, kind: "hello", payload: [] },
		{ id: 1, kind: "end", payload: 1 },
		{ id: 0, kind: "ping", payload: 1 },
		{ id: 1, kind: "credit" },
		{ id: 1, kind: "credit", credits: 0 },
		{ id: 1, kind: "credit", credits: 2 ** 32 },
		JSON.stringify({ id: 1, kind: "item", payload: "x".repeat(70000) }),
	];
	for (const frame of bad) {
		const data =
			typeof frame === "string" || frame instanceof Uint8Array
				? frame
				: JSON.stringify(frame);
		let refused = false;
		try {
			decodeServerFrame(data);
		} catch (error) {
			refused = error instanceof RpcTransportError;
		}
		assert(refused, `accepted ${String(data).slice(0, 120)}`);
	}
	// The frame limit counts UTF-8 bytes.
	let refused = false;
	try {
		decodeServerFrame(
			JSON.stringify({ id: 1, kind: "item", payload: "é".repeat(40000) }),
		);
	} catch {
		refused = true;
	}
	assert(refused, "a frame over 64 KiB of UTF-8");
	decodeServerFrame(
		JSON.stringify({ id: 1, kind: "item", payload: "é".repeat(30000) }),
	);
});

Deno.test("cursors, exits and keys", () => {
	assert(
		isCursor("c1.AQAAAAAAAAAH") &&
			!isCursor("c1.") &&
			!isCursor("c1.a=") &&
			!isCursor(1),
	);
	assert(isWireExit({ _tag: "Success", value: undefined }) === true);
	assert(
		!isWireExit(null) && !isWireExit({ _tag: "Failure", error: { _tag: 1 } }),
	);
	const keys = new Set(Array.from({ length: 100 }, () => randomKey()));
	assertEquals(keys.size, 100);
	for (const key of keys) assert(/^[A-Za-z0-9_-]{22}$/.test(key), key);
});

Deno.test("backoff grows exponentially with jitter, up to its cap", () => {
	assertEquals(
		[0, 1, 2, 3].map((n) => backoffDelay(n, {}, () => 0)),
		[125, 250, 500, 1000],
	);
	assertEquals(
		[0, 1, 2].map((n) => backoffDelay(n, {}, () => 0.9999)),
		[250, 500, 1000],
	);
	assertEquals(
		backoffDelay(40, { initialDelayMs: 100, maxDelayMs: 1000 }, () => 0.5),
		750,
	);
	const d = backoffDelay(3);
	assert(d >= 1000 && d <= 2000, String(d));
});

fakeTest(
	"a protocol violation drops the connection; the stream resumes on the next",
	async () => {
		const sockets: FakeSocket[] = [];
		const resumes: unknown[] = [];
		const connection = new RpcConnection({
			url: "http://127.0.0.1:1/rpc",
			transports: ["websocket"],
			reconnect: { initialDelayMs: 1, maxDelayMs: 1 },
			webSocket: () => {
				const first = sockets.length === 0;
				const socket = new FakeSocket(
					(frame, s) => {
						if (frame.kind === "call") {
							s.receive({
								id: frame.id,
								kind: "item",
								cursor: "c1.AgAAAAAAAAAB",
								payload: 1,
							});
							s.receive({
								id: frame.id,
								kind: "item",
								cursor: "c1.AgAAAAAAAAAC",
								payload: { bad: true },
								tag: "x",
							});
						}
						if (frame.kind === "resume") {
							resumes.push(frame.payload);
							s.hello(SESSION);
							s.receive({
								id: 1,
								kind: "item",
								cursor: "c1.AgAAAAAAAAAC",
								payload: 2,
							});
							s.receive({
								id: 1,
								kind: "exit",
								payload: { _tag: "Success", value: null },
							});
						}
					},
					first ? SESSION : "BBBBBBBBBBBBBBBBBBBBBB",
				);
				sockets.push(socket);
				return socket;
			},
		});
		const shop = new ShopV1Client(connection);
		assertEquals(await collect(shop.countdown(2)), [1, 2]);
		assertEquals(sockets.length, 2);
		assert(sockets[0].closed, "the violating socket is closed");
		assertEquals(resumes, [
			{ session: SESSION, calls: [{ id: 1, cursor: "c1.AgAAAAAAAAAB" }] },
		]);
		connection.close();
	},
);

fakeTest("a refused resume ends the stream with ResumeExpired", async () => {
	let n = 0;
	const connection = new RpcConnection({
		url: "http://127.0.0.1:1/rpc",
		transports: ["websocket"],
		reconnect: { initialDelayMs: 1 },
		webSocket: () => {
			n += 1;
			const socket = new FakeSocket(
				(frame, s) => {
					if (frame.kind === "call") s.close();
					if (frame.kind === "resume") s.hello("CCCCCCCCCCCCCCCCCCCCCC");
				},
				n === 1 ? SESSION : "CCCCCCCCCCCCCCCCCCCCCC",
			);
			return socket;
		},
	});
	const stream = new ShopV1Client(connection).countdown(5);
	assertEquals(await stream.exit, {
		_tag: "Failure",
		error: { _tag: "ResumeExpired" },
	});
	connection.close();
});

fakeTest(
	"a refused resume still delivers the items received but unread, then ResumeExpired",
	async () => {
		let n = 0;
		const resumes: unknown[] = [];
		const connection = new RpcConnection({
			url: "http://127.0.0.1:1/rpc",
			transports: ["websocket"],
			reconnect: { initialDelayMs: 1 },
			webSocket: () => {
				n += 1;
				return new FakeSocket(
					(frame, s) => {
						if (frame.kind === "call") {
							s.receive({
								id: frame.id,
								kind: "item",
								cursor: "c1.AgAAAAAAAAAB",
								payload: 5,
							});
							s.receive({
								id: frame.id,
								kind: "item",
								cursor: "c1.AgAAAAAAAAAC",
								payload: 4,
							});
							s.close();
						}
						if (frame.kind === "resume") {
							resumes.push(frame.payload);
							s.hello("CCCCCCCCCCCCCCCCCCCCCC");
						}
					},
					n === 1 ? SESSION : "CCCCCCCCCCCCCCCCCCCCCC",
				);
			},
		});
		const stream = new ShopV1Client(connection).countdown(5);
		await stream.exit;
		// Nothing was read: the resume asked for everything after sequence 0.
		assertEquals(resumes, [
			{ session: SESSION, calls: [{ id: 1, cursor: "c1.AgAAAAAAAAAA" }] },
		]);
		assertEquals(stream.cursor, undefined);
		const seen: unknown[] = [];
		const error = await assertRejects(
			(async () => {
				for await (const item of stream) seen.push(item);
			})(),
			(e) => e instanceof Error && e.name === "RpcFailureError",
		);
		assertEquals(seen, [5, 4]);
		assertEquals((error as { error: unknown }).error, {
			_tag: "ResumeExpired",
		});
		assertEquals(stream.cursor, "c1.AgAAAAAAAAAC");
		connection.close();
	},
);

fakeTest(
	"no WebSocket and no SSE: streams fail; client streams need a frame transport",
	async () => {
		const connection = new RpcConnection({
			url: "http://127.0.0.1:1/rpc",
			transports: ["websocket"],
			webSocket: () => {
				throw new Error("blocked");
			},
		});
		const shop = new ShopV1Client(connection);
		await assertRejects(
			collect(shop.countdown(1)),
			(e) => e instanceof RpcTransportError,
		);
		await assertRejects(shop.total([1]), (e) => e instanceof RpcTransportError);
		const wrong = new ShopV1Client({
			url: "http://127.0.0.1:1/rpc",
			transports: ["websocket"],
			webSocket: () => new FakeSocket(undefined, SESSION, "chat"),
		});
		await assertRejects(
			wrong.shout().exit,
			(e) => e instanceof RpcTransportError && /subprotocol/.test(e.message),
		);
		await delay(5);
	},
);
