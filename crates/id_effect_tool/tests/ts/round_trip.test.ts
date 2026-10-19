// The generated client against the test service of tests/rpc_gen_ts.rs: every kind, typed
// errors, refusals, versions, idempotency, the SSE fallback and cancellation.

import {
	RpcConnection,
	RpcFailureError,
	RpcProblemError,
	RpcTransportError,
	ShopV1Client,
	ShopV2Client,
} from "../golden/rpc/shop/client.ts";
import type {
	OrderId,
	ShopV1PlaceInput,
	Timestamp,
} from "../golden/rpc/shop/types.ts";
import {
	assert,
	assertEquals,
	assertRejects,
	collect,
	isAbort,
	recordingFetch,
	server,
	serverTest,
} from "./helpers.ts";

const order: ShopV1PlaceInput = {
	id: "o-0" as OrderId,
	placed_at: "2026-10-19T08:00:00Z" as Timestamp,
	lines: { tea: 1 },
	status: { _tag: "Open" },
};

serverTest("queries answer values and typed errors", async () => {
	const shop = new ShopV1Client({ url: server() });
	const tea = await shop.product(1);
	assert(tea._tag === "Success", JSON.stringify(tea));
	assertEquals(
		[tea.value.name, tea.value.price, tea.value.tags, tea.value.note ?? null],
		["Tea", { cents: 350, currency: "EUR" }, ["hot"], null],
	);
	assertEquals(await shop.product(2), {
		_tag: "Failure",
		error: { _tag: "SoldOut" },
	});
	assertEquals(await shop.product(9), {
		_tag: "Failure",
		error: { _tag: "NotFound", id: 9 },
	});
	const internal = await shop.product(3);
	assert(
		internal._tag === "Failure" && internal.error._tag === "InternalError",
		JSON.stringify(internal),
	);
	assert(
		!JSON.stringify(internal).includes("secret"),
		"an internal error carries no detail",
	);
	const o1 = await shop.order("o-1");
	assert(
		o1._tag === "Success" && o1.value.status._tag === "Shipped",
		JSON.stringify(o1),
	);
	assertEquals([o1.value.lines, o1.value.status.tracking], [{ tea: 2 }, "T-1"]);
	assertEquals(await shop.order("o-9"), {
		_tag: "Failure",
		error: { _tag: "UnknownOrder", id: "o-9" },
	});
	const invalid = await shop.order(7 as unknown as string);
	assert(
		invalid._tag === "Failure" && invalid.error._tag === "InvalidInput",
		JSON.stringify(invalid),
	);
	shop.connection.close();
});

serverTest("versions are served side by side on one connection", async () => {
	const connection = new RpcConnection({ url: server() });
	const v1 = new ShopV1Client(connection);
	const v2 = new ShopV2Client(connection);
	assertEquals(
		[ShopV1Client.version, ShopV2Client.version, ShopV2Client.service],
		[1, 2, "Shop"],
	);
	assertEquals(await v2.names(), { _tag: "Success", value: ["Tea", "Coffee"] });
	assertEquals((await v1.sleep(0))._tag, "Success");
	connection.close();
});

serverTest(
	"mutations: guards refuse with a Problem, Idempotency-Key replays",
	async () => {
		const { fetch, requests } = recordingFetch();
		const shop = new ShopV1Client({ url: server(), fetch });
		const refused = await assertRejects(
			shop.restock(1),
			(e) => e instanceof RpcProblemError && e.status === 403,
		);
		assertEquals((refused as RpcProblemError).problem.status ?? 403, 403);
		const staff = new ShopV1Client({
			url: server(),
			headers: () => Promise.resolve({ "X-Staff": "yes" }),
		});
		assertEquals(await staff.restock(4), { _tag: "Success", value: 4 });
		// A replayed key answers the stored result; fresh keys run the mutation again.
		const key = "replay-0123456789abcdef";
		const first = await shop.place(order, { idempotencyKey: key });
		const again = await shop.place(order, { idempotencyKey: key });
		assert(first._tag === "Success" && again._tag === "Success");
		assertEquals(again.value.id, first.value.id);
		const fresh = await shop.place(order);
		assert(
			fresh._tag === "Success" && fresh.value.id !== first.value.id,
			JSON.stringify(fresh),
		);
		await assertRejects(
			shop.place(order, { idempotencyKey: "short" }),
			(e) => e instanceof RangeError,
		);
		// Every request says x-rpc: 1; every mutation carries a key of at least 16 characters.
		for (const request of requests) {
			assertEquals(request.headers["x-rpc"], "1");
			if (
				request.body?.includes("Shop.place") ||
				request.body?.includes("Shop.restock")
			) {
				assert(
					(request.headers["idempotency-key"] ?? "").length >= 16,
					JSON.stringify(request.headers),
				);
			}
		}
		assertEquals(
			requests.filter((r) => r.headers["idempotency-key"] === key).length,
			2,
		);
		shop.connection.close();
		staff.connection.close();
	},
);

serverTest(
	"streams over WebSocket: server, client, two-way, subscription",
	async () => {
		const shop = new ShopV1Client({ url: server() });
		assertEquals(await collect(shop.countdown(3)), [3, 2, 1]);
		assertEquals(shop.connection.transport, "websocket");
		const failing = shop.countdown(-1);
		const seen: number[] = [];
		const error = await assertRejects(
			(async () => {
				for await (const n of failing) seen.push(n);
			})(),
			(e) => e instanceof RpcFailureError,
		);
		assertEquals(
			[seen, (error as RpcFailureError<unknown>).error],
			[[1], { _tag: "NotFound", id: -1 }],
		);
		assertEquals(await failing.exit, {
			_tag: "Failure",
			error: { _tag: "NotFound", id: -1 },
		});
		assertEquals(await shop.total([1, 2, 3]), { _tag: "Success", value: 6 });
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
		const after = ticks.cursor;
		assert(after !== undefined && after.startsWith("c1."), String(after));
		const third = shop.ticks(10, { cursor: "c1.AQAAAAAAAAAM" });
		assertEquals(await collect(third), [13, 14]);
		assertEquals(await collect(shop.ticks(10, { cursor: after })), []);
		shop.connection.close();
	},
);

serverTest(
	"without WebSocket, server streams and subscriptions use SSE",
	async () => {
		const shop = new ShopV1Client({ url: server(), transports: ["sse"] });
		assertEquals(await collect(shop.countdown(3)), [3, 2, 1]);
		assertEquals(shop.connection.transport, "sse");
		const ticks = shop.ticks(10);
		assertEquals(await collect(ticks), [10, 11, 12, 13, 14]);
		assertEquals(
			await collect(shop.ticks(10, { cursor: "c1.AQAAAAAAAAAL" })),
			[12, 13, 14],
		);
		assertEquals((await shop.countdown(-1).exit)._tag, "Failure");
		await assertRejects(shop.total([1]), (e) => e instanceof RpcTransportError);
		await assertRejects(
			shop.shout().send("a"),
			(e) => e instanceof RpcTransportError,
		);
		// No frame transport refuses: a WebSocket that cannot open falls back to SSE too.
		const blocked = new ShopV1Client({
			url: server(),
			webSocket: () => {
				throw new Error("no sockets here");
			},
		});
		assertEquals(await collect(blocked.countdown(2)), [2, 1]);
		assertEquals(blocked.connection.transport, "sse");
		shop.connection.close();
		blocked.connection.close();
	},
);

serverTest("AbortSignal cancels queries and streams", async () => {
	const shop = new ShopV1Client({ url: server() });
	const started = Date.now();
	const controller = new AbortController();
	const slow = shop.sleep(5000, { signal: controller.signal });
	setTimeout(() => controller.abort(), 30);
	await assertRejects(slow, isAbort);
	assert(Date.now() - started < 2000, "the abort did not wait for the server");
	const unbatched = new AbortController();
	const alone = shop.sleep(5000, { signal: unbatched.signal, batch: false });
	setTimeout(() => unbatched.abort(), 30);
	await assertRejects(alone, isAbort);
	const before = new AbortController();
	before.abort();
	await assertRejects(shop.sleep(1, { signal: before.signal }), isAbort);
	await assertRejects(
		collect(shop.countdown(5, { signal: before.signal })),
		isAbort,
	);
	// A long stream, cancelled after three items (the iterator's `return` cancels the call).
	const items: number[] = [];
	for await (const n of shop.countdown(1_000_000)) {
		items.push(n);
		if (items.length === 3) break;
	}
	assertEquals(items, [1_000_000, 999_999, 999_998]);
	const streamAbort = new AbortController();
	const stream = shop.countdown(1_000_000, { signal: streamAbort.signal });
	await assertRejects(
		(async () => {
			for await (const _ of stream) streamAbort.abort();
		})(),
		isAbort,
	);
	await assertRejects(stream.exit, isAbort);
	// The connection still serves calls.
	assertEquals(await collect(shop.countdown(2)), [2, 1]);
	shop.connection.close();
});
