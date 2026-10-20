// Calls made in one microtask go out as one batch (design note §2.2), like the Rust
// `RpcClient::call`: against the test server, and against a scripted fetch for the limits.

import {
	MAX_BATCH_CALLS,
	RpcConnection,
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
	echo,
	json,
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

function bodyOf(
	body: string | undefined,
): { id: number; tag: string; v: number }[] {
	const parsed = JSON.parse(body ?? "null");
	return Array.isArray(parsed) ? parsed : [parsed];
}

serverTest("calls made in one microtask go out as one batch", async () => {
	const { fetch, requests } = recordingFetch();
	const connection = new RpcConnection({ url: server(), fetch });
	const shop = new ShopV1Client(connection);
	const v2 = new ShopV2Client(connection);
	const [tea, sold, o1, names] = await Promise.all([
		shop.product(1),
		shop.product(2),
		shop.order("o-1"),
		v2.names(),
	]);
	assertEquals(requests.length, 1);
	const calls = bodyOf(requests[0].body);
	assertEquals(
		calls.map((c) => [c.tag, c.v]),
		[
			["Shop.product", 1],
			["Shop.product", 1],
			["Shop.order", 1],
			["Shop.names", 2],
		],
	);
	assertEquals(new Set(calls.map((c) => c.id)).size, 4);
	assertEquals(requests[0].headers["x-rpc"], "1");
	assert(
		requests[0].headers["idempotency-key"] === undefined,
		"no key without a mutation",
	);
	assert(tea._tag === "Success" && tea.value.name === "Tea");
	assertEquals(sold, { _tag: "Failure", error: { _tag: "SoldOut" } });
	assertEquals(
		[o1._tag, names],
		["Success", { _tag: "Success", value: ["Tea", "Coffee"] }],
	);
	// Calls in separate ticks are separate requests; a single call is a plain envelope.
	await shop.product(1);
	await shop.product(1);
	assertEquals(requests.length, 3);
	assert(!requests[2].body?.startsWith("["), requests[2].body);
	connection.close();
});

serverTest(
	"a batch with two mutations is ordered and carries one key",
	async () => {
		const { fetch, requests } = recordingFetch();
		const shop = new ShopV1Client({ url: server(), fetch });
		const [a, b] = await Promise.all([shop.place(order), shop.place(order)]);
		assertEquals(requests.length, 1);
		assertEquals(requests[0].headers["x-rpc-ordered"], "true");
		assert(
			(requests[0].headers["idempotency-key"] ?? "").length >= 22,
			"a 128-bit key",
		);
		assert(
			a._tag === "Success" && b._tag === "Success",
			JSON.stringify([a, b]),
		);
		assert(
			Number(b.value.id.slice(2)) === Number(a.value.id.slice(2)) + 1,
			"run in call order",
		);
		shop.connection.close();
	},
);

serverTest("a refusal applies to the whole batch", async () => {
	const shop = new ShopV1Client({ url: server() });
	const [restock, product] = await Promise.allSettled([
		shop.restock(1),
		shop.product(1),
	]);
	assert(
		restock.status === "rejected" && restock.reason instanceof RpcProblemError,
		String(restock),
	);
	assert(
		product.status === "rejected" && product.reason instanceof RpcProblemError,
		String(product),
	);
	assertEquals((product.reason as RpcProblemError).status, 403);
	shop.connection.close();
});

Deno.test("batches split at 50 calls and at the byte cap", async () => {
	const { fetch, requests } = recordingFetch(echo);
	const shop = new ShopV1Client({ url: "http://127.0.0.1:1/rpc", fetch });
	const results = await Promise.all(
		Array.from({ length: 120 }, (_, i) => shop.product(i)),
	);
	assertEquals(
		requests.map((r) => bodyOf(r.body).length),
		[MAX_BATCH_CALLS, MAX_BATCH_CALLS, 20],
	);
	assertEquals(
		results.map((r) => (r._tag === "Success" ? r.value : null)),
		Array.from({ length: 120 }, (_, i) => i),
	);
	const small = recordingFetch(echo);
	const capped = new ShopV1Client({
		url: "http://127.0.0.1:1/rpc",
		fetch: small.fetch,
		maxBatchBytes: 200,
		maxBatchCalls: 3,
	});
	await Promise.all(
		["a".repeat(80), "b".repeat(80), "c", "d", "e", "f"].map((s) =>
			capped.order(s),
		),
	);
	assertEquals(
		small.requests.map((r) => bodyOf(r.body).length),
		[1, 2, 3],
	);
	// Different deadlines go in different requests, each with its header.
	const timed = recordingFetch(echo);
	const deadline = new ShopV1Client({
		url: "http://127.0.0.1:1/rpc",
		fetch: timed.fetch,
	});
	await Promise.all([
		deadline.product(1, { deadlineMs: 500 }),
		deadline.product(2),
		deadline.product(3, { deadlineMs: 500 }),
	]);
	assertEquals(
		timed.requests.map((r) => [
			bodyOf(r.body).length,
			r.headers["x-rpc-deadline-ms"] ?? null,
		]),
		[
			[2, "500"],
			[1, null],
		],
	);
});

Deno.test("batch: false and an explicit Idempotency-Key send a call alone", async () => {
	const { fetch, requests } = recordingFetch(echo);
	const shop = new ShopV1Client({ url: "http://127.0.0.1:1/rpc", fetch });
	await Promise.all([
		shop.product(1, { batch: false }),
		shop.place(order, { idempotencyKey: "k".repeat(16) }),
		shop.product(2),
		shop.product(3),
	]);
	assertEquals(
		requests.map((r) => bodyOf(r.body).length),
		[1, 1, 2],
	);
	assertEquals(requests[1].headers["idempotency-key"], "k".repeat(16));
	assertEquals(requests[1].headers["x-rpc-ordered"], undefined);
});

Deno.test("answers that do not match their calls fail every call", async () => {
	const answers: ((body: unknown[]) => Response)[] = [
		() => json([]),
		(calls) =>
			json(
				calls.map(() => ({
					id: 999,
					tag: "Shop.product",
					exit: { _tag: "Success", value: 1 },
				})),
			),
		(calls) =>
			json(calls.map((c) => ({ ...(c as object), exit: { _tag: "Nope" } }))),
		() =>
			new Response("{", { headers: { "content-type": "application/json" } }),
		() => new Response("hi", { headers: { "content-type": "text/html" } }),
		() =>
			new Response("x".repeat(100), {
				headers: { "content-type": "application/json" },
			}),
	];
	for (const answer of answers) {
		const { fetch } = recordingFetch((r) => answer(bodyOf(r.body)));
		const shop = new ShopV1Client({
			url: "http://127.0.0.1:1/rpc",
			fetch,
			maxResponseBytes: 64,
		});
		const settled = await Promise.allSettled([
			shop.product(1),
			shop.product(2),
		]);
		for (const s of settled) {
			assert(
				s.status === "rejected" && s.reason instanceof RpcTransportError,
				String(s.status === "rejected" ? s.reason : "resolved"),
			);
		}
	}
	const problem = recordingFetch(() =>
		json(
			{ title: "Too Many Requests", status: 429 },
			429,
			"application/problem+json",
		),
	);
	const limited = new ShopV1Client({
		url: "http://127.0.0.1:1/rpc",
		fetch: problem.fetch,
	});
	const refused = await assertRejects(
		limited.product(1),
		(e) => e instanceof RpcProblemError && e.status === 429,
	);
	assertEquals((refused as RpcProblemError).problem.title, "Too Many Requests");
	// A guard's bare status (no Problem body) is a refusal too.
	const bare = recordingFetch(() => new Response(null, { status: 403 }));
	const guarded = new ShopV1Client({
		url: "http://127.0.0.1:1/rpc",
		fetch: bare.fetch,
	});
	const forbidden = await assertRejects(
		guarded.product(1),
		(e) => e instanceof RpcProblemError && e.status === 403,
	);
	assertEquals((forbidden as RpcProblemError).problem, { status: 403 });
	const broken = new ShopV1Client({
		url: "http://127.0.0.1:1/rpc",
		fetch: () => Promise.reject(new TypeError("offline")),
	});
	await assertRejects(
		broken.product(1),
		(e) => e instanceof RpcTransportError && /offline/.test(e.message),
	);
	let threw = false;
	try {
		new ShopV1Client({ url: "not a url" });
	} catch {
		threw = true;
	}
	assert(threw, "a malformed URL is refused");
	threw = false;
	try {
		await limited.connection.unary("bad tag", 1, "query", null);
	} catch (error) {
		threw = error instanceof RangeError;
	}
	assert(threw, "a malformed tag is refused");
});
