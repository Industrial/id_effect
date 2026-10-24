// The generated client over WebTransport under Deno (F45, r13-f-webtransport-host): the shop on
// the host entry point of tests/rpc_gen_ts.rs (`ServerHandle` with `server.http3.webtransport`),
// HTTPS and HTTP/3 on one port, a 13-day ECDSA P-256 certificate pinned through
// `serverCertificateHashes`. Deno's `WebTransport` needs `--unstable-net`.
//
// `deno test --unstable-net --cert <ca.pem> … -- <rpc url> <webtransport url> <sha-256 hex>`;
// skipped without the arguments.

import { RpcConnection, ShopV1Client } from "../golden/rpc/shop/client.ts";
import {
	assert,
	assertEquals,
	collect,
	server,
	serverTest,
} from "./helpers.ts";

/** The session URL (`https://127.0.0.1:PORT/rpc/wt`). */
const sessionUrl: string | undefined = Deno.args[1];
/** The certificate's SHA-256, hex. */
const certificateHash: string | undefined = Deno.args[2];

type Session = {
	readonly closed: Promise<unknown>;
	close(info?: { closeCode?: number; reason?: string }): void;
};
type SessionConstructor = new (
	url: string,
	options?: Record<string, unknown>,
) => Session;

const global = globalThis as { WebTransport?: SessionConstructor };
const native = global.WebTransport;

/** Every session the client opens, through a wrapper around the global `WebTransport`. */
const sessions: Session[] = [];
if (native) {
	global.WebTransport = class extends native {
		constructor(url: string, options?: Record<string, unknown>) {
			super(url, options);
			sessions.push(this);
		}
	};
}

function hash(): Uint8Array<ArrayBuffer> {
	const hex = certificateHash ?? "";
	assert(/^[0-9a-f]{64}$/.test(hex), `a SHA-256 in hex: ${hex}`);
	const bytes = new Uint8Array(new ArrayBuffer(32));
	for (let i = 0; i < 32; i++) {
		bytes[i] = parseInt(hex.slice(2 * i, 2 * i + 2), 16);
	}
	return bytes;
}

/** A client whose streams may only use WebTransport (no WebSocket or SSE to fall back on). */
function client(): ShopV1Client {
	assert(sessionUrl !== undefined, "no WebTransport URL");
	return new ShopV1Client(
		new RpcConnection({
			url: server(),
			transports: ["webtransport"],
			webTransport: {
				url: sessionUrl,
				serverCertificateHashes: [{ algorithm: "sha-256", value: hash() }],
			},
			reconnect: { initialDelayMs: 20, maxDelayMs: 200, maxAttempts: 5 },
		}),
	);
}

serverTest("WebTransport is a global under --unstable-net", () => {
	assert(typeof native === "function", "no WebTransport global");
	return Promise.resolve();
});

serverTest(
	"generated_client_webtransport_round_trip_deno: every kind over one session",
	async () => {
		const opened = sessions.length;
		const shop = client();
		// Queries ride HTTPS (TCP); every stream rides the WebTransport session.
		const tea = await shop.product(1);
		assert(
			tea._tag === "Success" && tea.value.name === "Tea",
			JSON.stringify(tea),
		);
		assertEquals(await collect(shop.countdown(3)), [3, 2, 1]);
		assertEquals(shop.connection.transport, "webtransport");
		assertEquals(await shop.countdown(-1).exit, {
			_tag: "Failure",
			error: { _tag: "NotFound", id: -1 },
		});
		async function* numbers() {
			for (let i = 1; i <= 100; i++) yield i;
		}
		assertEquals(await shop.total(numbers()), { _tag: "Success", value: 5050 });
		const shout = shop.shout();
		await shout.send("a");
		await shout.send("b");
		shout.end();
		assertEquals(await collect(shout), ["A", "B"]);
		assertEquals(await shout.exit, { _tag: "Success", value: null });
		const ticks = shop.ticks(10);
		assertEquals(await collect(ticks), [10, 11, 12, 13, 14]);
		assertEquals(
			await collect(shop.ticks(10, { cursor: "c1.AQAAAAAAAAAM" })),
			[13, 14],
		);
		// Calls at once, each on its own stream of the one session.
		const [a, b] = await Promise.all([
			collect(shop.countdown(50)),
			collect(shop.countdown(40)),
		]);
		assertEquals([a.length, b.length, a[49], b[0]], [50, 40, 1, 40]);
		assertEquals(sessions.length - opened, 1);
		shop.connection.close();
	},
);

serverTest(
	"a lost WebTransport session reconnects and the stream resumes",
	async () => {
		const opened = sessions.length;
		const shop = client();
		const stream = shop.countdown(200);
		const iterator = stream[Symbol.asyncIterator]();
		const read: number[] = [];
		for (let i = 0; i < 5; i++) {
			const step = await iterator.next();
			assert(!step.done, `ended after ${i} items`);
			read.push(step.value);
		}
		// The session goes away under the client: it opens a new one and resumes from the
		// cursor of the last item it read.
		sessions[sessions.length - 1].close({ closeCode: 0, reason: "lost" });
		for (;;) {
			const step = await iterator.next();
			if (step.done) break;
			read.push(step.value);
		}
		assertEquals(
			read,
			Array.from({ length: 200 }, (_, i) => 200 - i),
			"every item once, in order",
		);
		assertEquals(await stream.exit, { _tag: "Success", value: null });
		assertEquals(sessions.length - opened, 2);
		assertEquals(shop.connection.transport, "webtransport");
		shop.connection.close();
	},
);
