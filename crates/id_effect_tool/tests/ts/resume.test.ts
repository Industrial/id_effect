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
 * then nothing more). Records every frame the client sends; `reopened` resolves when the
 * client opens its second socket. */
function cutting(dropAfter: number): {
	factory: (url: string, protocols: string[]) => WebSocketLike;
	opened: () => number;
	reopened: Promise<void>;
	sent: Record<string, unknown>[];
} {
	let opened = 0;
	let reopen = () => {};
	const reopened = new Promise<void>((resolve) => {
		reopen = resolve;
	});
	const sent: Record<string, unknown>[] = [];
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
					if (cut && frame.kind === "item" && ++items === dropAfter) {
						dropped = true;
						socket.close();
					}
				});
			},
		};
	};
	return { factory, opened: () => opened, reopened, sent };
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
