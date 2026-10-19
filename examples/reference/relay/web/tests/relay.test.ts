// The generated TypeScript client (`web/src/rpc`, `id-effect rpc gen ts`) against a running
// Relay node: unary calls over HTTP with a bearer token, a typed failure, a room joined and a
// message posted over the RPC WebSocket, and the history resumed from a cursor.
//
// `deno test … web/tests/relay.test.ts -- <https://host:port/rpc> <token>`; tests/contract.rs
// starts the nodes and passes both. Without them the test is ignored.

import { RelayV1Client, RpcConnection } from "../src/rpc/client.ts";

const [url, token] = Deno.args;

function assert(cond: unknown, message = "assertion failed"): asserts cond {
	if (!cond) throw new Error(message);
}

Deno.test({
	name: "the generated client chats with a Relay node",
	ignore: url === undefined || token === undefined,
	sanitizeOps: false,
	sanitizeResources: false,
	fn: async () => {
		const connection = new RpcConnection({
			url,
			headers: { authorization: `Bearer ${token}` },
			transports: ["websocket"],
			// Browsers and Deno cannot set headers on a WebSocket: the token rides in the query
			// of the upgrade (the node moves it into the header).
			webSocket: (socketUrl, protocols) => {
				const u = new URL(socketUrl);
				u.searchParams.set("access_token", token);
				return new WebSocket(u.toString(), protocols);
			},
		});
		const relay = new RelayV1Client(connection);

		// Unary over HTTP: the cluster as this node sees it, and a typed failure.
		const cluster = await relay.cluster();
		assert(cluster._tag === "Success", JSON.stringify(cluster));
		assert(cluster.value.nodes.length === 2, JSON.stringify(cluster));
		assert(
			cluster.value.nodes.every((n) => n.up),
			JSON.stringify(cluster),
		);
		const exists = await relay.create_room("general");
		assert(
			exists._tag === "Failure" && exists.error._tag === "RoomExists",
			JSON.stringify(exists),
		);

		// The room over the WebSocket: joined, then our own message comes back through it.
		const join = relay.join({ room: "general", status: "online" });
		const events = join[Symbol.asyncIterator]();
		const joined = await events.next();
		assert(
			!joined.done && joined.value.kind === "joined",
			JSON.stringify(joined),
		);
		const chat = relay.chat();
		await chat.send({ room: "general", text: "hello from deno" });
		const acks = chat[Symbol.asyncIterator]();
		const ack = await acks.next();
		assert(!ack.done && ack.value.room === "general", JSON.stringify(ack));
		for (;;) {
			const event = await events.next();
			assert(!event.done, "the room ended");
			if (event.value.message) {
				assert(
					event.value.message.text === "hello from deno",
					JSON.stringify(event.value),
				);
				assert(
					event.value.message.seq === ack.value.seq,
					JSON.stringify(event.value),
				);
				break;
			}
		}

		// The history: the message Rust posted first, then ours, in order.
		const history = relay.history("general");
		const items = history[Symbol.asyncIterator]();
		const texts: string[] = [];
		for (const _ of [1, 2]) {
			const item = await items.next();
			assert(!item.done, "the history ended");
			texts.push(item.value.text);
		}
		assert(
			texts.join("|") === "hello from rust|hello from deno",
			JSON.stringify(texts),
		);
		// `cursor` is the last item the stream received: resume after both, once a third is in.
		const cursor = history.cursor;
		assert(typeof cursor === "string", "a cursor");
		history.cancel();
		await chat.send({ room: "general", text: "third" });
		const third = await acks.next();
		assert(!third.done, "no acknowledgement");
		const resumed = relay.history("general", { cursor });
		const next = await resumed[Symbol.asyncIterator]().next();
		assert(
			!next.done &&
				next.value.text === "third" &&
				next.value.seq === third.value.seq,
			JSON.stringify(next),
		);
		resumed.cancel();
		chat.end();
		join.cancel();
	},
});
