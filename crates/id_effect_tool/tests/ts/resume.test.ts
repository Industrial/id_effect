// A dropped connection reconnects with backoff and resumes (web.realtime#7): server streams and
// two-way streams from their last cursor (`resume` frame), subscriptions after their last event.
// The test server is real; the socket is cut by a wrapper after a number of items.

import { RpcConnection, ShopV1Client } from "../golden/rpc/shop/client.ts";
import type { WebSocketLike } from "../golden/rpc/shop/client.ts";
import {
	assert,
	assertEquals,
	collect,
	server,
	serverTest,
} from "./helpers.ts";

/** Real WebSockets; the first one is cut after `dropAfter` item frames (it delivers those,
 * then nothing more). Records every frame the client sends and every item frame the first
 * socket delivered; `dropped` resolves when it is cut, `reopened` when the client opens its
 * second socket, `resent` when it sends its first frame on it (the `resume` or `call`). */
function cutting(dropAfter: number): {
	factory: (url: string, protocols: string[]) => WebSocketLike;
	opened: () => number;
	dropped: Promise<void>;
	reopened: Promise<void>;
	resent: Promise<void>;
	sent: Record<string, unknown>[];
	received: Record<string, unknown>[];
} {
	let opened = 0;
	let reopen = () => {};
	const reopened = new Promise<void>((resolve) => {
		reopen = resolve;
	});
	let resend = () => {};
	const resent = new Promise<void>((resolve) => {
		resend = resolve;
	});
	let drop = () => {};
	const dropped = new Promise<void>((resolve) => {
		drop = resolve;
	});
	const sent: Record<string, unknown>[] = [];
	const received: Record<string, unknown>[] = [];
	const factory = (url: string, protocols: string[]): WebSocketLike => {
		opened += 1;
		const cut = opened === 1;
		if (opened === 2) reopen();
		const socket = new WebSocket(url, protocols);
		let items = 0;
		let dropped = false;
		return {
			get protocol() {
				return socket.protocol;
			},
			get readyState() {
				return socket.readyState;
			},
			send: (data: string) => {
				sent.push(JSON.parse(data));
				if (!cut) resend();
				if (!dropped) socket.send(data);
			},
			close: (code?: number, reason?: string) => socket.close(code, reason),
			addEventListener: (type, listener) => {
				if (type !== "message") {
					socket.addEventListener(type, listener);
					return;
				}
				socket.addEventListener("message", (event) => {
					if (dropped) return;
					const frame = JSON.parse(String(event.data));
					listener(event);
					if (cut && frame.kind === "item") received.push(frame);
					if (cut && frame.kind === "item" && ++items === dropAfter) {
						dropped = true;
						socket.close();
						drop();
					}
				});
			},
		};
	};
	return {
		factory,
		opened: () => opened,
		dropped,
		reopened,
		resent,
		sent,
		received,
	};
}

/** Take `n` items from `iterator`. */
async function take<T>(iterator: AsyncIterator<T>, n: number): Promise<T[]> {
	const out: T[] = [];
	for (let i = 0; i < n; i++) {
		const step = await iterator.next();
		assert(!step.done, `ended after ${i} of ${n} items`);
		out.push(step.value);
	}
	return out;
}

/** Every remaining item of `iterator`. */
async function rest<T>(iterator: AsyncIterator<T>): Promise<T[]> {
	const out: T[] = [];
	for (;;) {
		const step = await iterator.next();
		if (step.done) return out;
		out.push(step.value);
	}
}

function client(
	factory: (url: string, protocols: string[]) => WebSocketLike,
): ShopV1Client {
	return new ShopV1Client(
		new RpcConnection({
			url: server(),
			transports: ["websocket"],
			webSocket: factory,
			reconnect: { initialDelayMs: 20, maxDelayMs: 200 },
		}),
	);
}

serverTest(
	"a server stream resumes after the socket drops: every item once, in order",
	async () => {
		const cut = cutting(5);
		const shop = client(cut.factory);
		const items = await collect(shop.countdown(100));
		assertEquals(
			items,
			Array.from({ length: 100 }, (_, i) => 100 - i),
		);
		assertEquals(cut.opened(), 2);
		const resume = cut.sent.find((f) => f.kind === "resume");
		assert(resume !== undefined, "a resume frame was sent");
		const payload = resume.payload as {
			session: string;
			calls: { id: number; cursor: string }[];
		};
		assert(/^[A-Za-z0-9_-]{22}$/.test(payload.session), payload.session);
		assertEquals(payload.calls.length, 1);
		assert(
			payload.calls[0].cursor.startsWith("c1.Ag"),
			payload.calls[0].cursor,
		);
		// The resume is the first frame on the new connection.
		const afterResume = cut.sent.slice(cut.sent.indexOf(resume) + 1);
		assert(
			!afterResume.some(
				(f) => f.kind === "call" && f.id === payload.calls[0].id,
			),
			"the call is not reopened",
		);
		assertEquals(await shop.countdown(1).exit, {
			_tag: "Success",
			value: null,
		});
		shop.connection.close();
	},
);

serverTest("a two-way stream resumes and keeps sending", async () => {
	const cut = cutting(2);
	const shop = client(cut.factory);
	const shout = shop.shout();
	const seen: string[] = [];
	await shout.send("a");
	await shout.send("b");
	const iterator = shout[Symbol.asyncIterator]();
	seen.push((await iterator.next()).value as string);
	seen.push((await iterator.next()).value as string);
	// The socket is cut after the second item. Input sent before the client notices is lost
	// (at most once across a reconnect); once it reconnects, a send waits for the window the
	// resumed call's first credit frame sets.
	await cut.reopened;
	await shout.send("c");
	shout.end();
	for (;;) {
		const step = await iterator.next();
		if (step.done) break;
		seen.push(step.value as string);
	}
	assertEquals(seen, ["A", "B", "C"]);
	assertEquals(cut.opened(), 2);
	assertEquals(await shout.exit, { _tag: "Success", value: null });
	shop.connection.close();
});

serverTest("a subscription reopens after its last event cursor", async () => {
	const cut = cutting(2);
	const shop = client(cut.factory);
	const ticks = shop.ticks(10);
	assertEquals(await collect(ticks), [10, 11, 12, 13, 14]);
	assertEquals(cut.opened(), 2);
	const calls = cut.sent.filter(
		(f) => f.kind === "call" && f.tag === "Shop.ticks",
	);
	assertEquals(calls.length, 2);
	assertEquals(calls[1].cursor, "c1.AQAAAAAAAAAL");
	shop.connection.close();
});

// F58: the cursor a resume sends is the last item the caller READ, not the last one received.
// The socket drops with items received but unread; they come again after the resume, exactly
// once and in order, and the server counts only the read ones as credited.

serverTest(
	"resume_does_not_skip_buffered_unread_items: server stream",
	async () => {
		const cut = cutting(8);
		const shop = client(cut.factory);
		const stream = shop.countdown(20);
		const iterator = stream[Symbol.asyncIterator]();
		// Eight items arrive and wait unread; then the socket drops and the caller reads three.
		await cut.dropped;
		assertEquals(stream.cursor, undefined, "nothing read yet");
		const read = await take(iterator, 3);
		assertEquals(read, [20, 19, 18]);
		assertEquals(stream.cursor, cut.received[2].cursor);
		// Five items stay unread until the resume has gone out.
		await cut.resent;
		const after = await rest(iterator);
		assertEquals(
			[...read, ...after],
			Array.from({ length: 20 }, (_, i) => 20 - i),
			"every item once, in order",
		);
		const resume = cut.sent.find((f) => f.kind === "resume");
		assert(resume !== undefined, "a resume frame was sent");
		const calls = (resume.payload as { calls: { cursor: string }[] }).calls;
		assertEquals(calls.length, 1);
		assertEquals(
			calls[0].cursor,
			cut.received[2].cursor,
			"resumed after the third item, the last one read",
		);
		assert(
			calls[0].cursor !== cut.received[7].cursor,
			"not after the eighth item, the last one received",
		);
		// Credits: none of the unread items was credited before the drop.
		const before = cut.sent.slice(0, cut.sent.indexOf(resume));
		assert(
			!before.some((f) => f.kind === "credit"),
			"no credit before the resume",
		);
		assertEquals(await stream.exit, { _tag: "Success", value: null });
		shop.connection.close();
	},
);

serverTest(
	"resume_does_not_skip_buffered_unread_items: subscription",
	async () => {
		const cut = cutting(4);
		const shop = client(cut.factory);
		const ticks = shop.ticks(10);
		const iterator = ticks[Symbol.asyncIterator]();
		await cut.dropped;
		const read = await take(iterator, 2);
		assertEquals(read, [10, 11]);
		assertEquals(ticks.cursor, "c1.AQAAAAAAAAAL");
		// Two events stay unread until the subscription has been reopened.
		await cut.resent;
		const after = await rest(iterator);
		assertEquals(
			[...read, ...after],
			[10, 11, 12, 13, 14],
			"every event once, in order",
		);
		const calls = cut.sent.filter(
			(f) => f.kind === "call" && f.tag === "Shop.ticks",
		);
		assertEquals(calls.length, 2);
		// Reopened after event 11 (read), not 13 (the last one received).
		assertEquals(calls[1].cursor, "c1.AQAAAAAAAAAL");
		assertEquals(ticks.cursor, "c1.AQAAAAAAAAAO");
		shop.connection.close();
	},
);

serverTest(
	"resume_does_not_skip_buffered_unread_items: subscription over SSE",
	async () => {
		// The first event stream delivers four events and ends before its exit; the client
		// reconnects with `Last-Event-ID`.
		const requests: Record<string, string>[] = [];
		let reconnected = () => {};
		const second = new Promise<void>((resolve) => {
			reconnected = resolve;
		});
		const fetch = async (input: string, init: RequestInit) => {
			const headers: Record<string, string> = {};
			new Headers(init.headers).forEach((value, name) => {
				headers[name] = value;
			});
			requests.push(headers);
			const response = await globalThis.fetch(input, init);
			if (requests.length > 1) {
				reconnected();
				return response;
			}
			const events = (await response.text())
				.split("\n\n")
				.filter((e) => e.includes("event: item"))
				.slice(0, 4);
			assertEquals(events.length, 4);
			return new Response(`${events.join("\n\n")}\n\n`, {
				status: response.status,
				headers: response.headers,
			});
		};
		const shop = new ShopV1Client(
			new RpcConnection({
				url: server(),
				transports: ["sse"],
				fetch,
				reconnect: { initialDelayMs: 20, maxDelayMs: 200 },
			}),
		);
		const ticks = shop.ticks(10);
		const iterator = ticks[Symbol.asyncIterator]();
		const read = await take(iterator, 2);
		assertEquals(read, [10, 11]);
		await second;
		const after = await rest(iterator);
		assertEquals([...read, ...after], [10, 11, 12, 13, 14]);
		assertEquals(requests.length, 2);
		assertEquals(requests[0]["last-event-id"], undefined);
		assertEquals(requests[1]["last-event-id"], "c1.AQAAAAAAAAAL");
		shop.connection.close();
	},
);
