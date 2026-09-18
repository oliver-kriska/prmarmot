// The Swift half of the FFI contract.
//
// `ffi/tests/offline_board.rs` proves the bridge works from Rust. This proves
// the generated bindings work from Swift, in the simulator, with a transport
// written the way the app will write one: async, URLSession-shaped, and
// holding its client `weak`.
//
// No network. Every response comes from the same fixtures the Rust goldens
// use, so a disagreement between the two sides shows up as a different number
// here rather than as a mystery in the app.

import XCTest
@testable import PRMarmotCore

/// 2026-07-26T12:00:00Z — the instant `core/tests/golden/search.json` is
/// pinned at, so `is:stale` is a property of the fixture and not of today.
private let now: Int64 = 1_785_067_200

/// The shape a real `URLSession` transport has, with the data task replaced by
/// a file read. Note the `weak` client: see `ffi/README.md`.
private final class OfflineTransport: GithubTransport, @unchecked Sendable {
    /// Never strong. `BoardClient` owns this object; a strong reference back
    /// is a retain cycle neither ARC nor Rust can see.
    weak var client: BoardClient?

    private let body: String
    private let status: UInt16
    private(set) var seen: [GraphqlRequest] = []

    init(fixture: String) throws {
        self.body = try Self.read(fixture)
        self.status = 200
    }

    /// The review queue asks for two aliases in one operation, which the
    /// committed fixture (a single legacy search) predates. Reshaping it here
    /// keeps one set of fixtures for the whole project — these files are
    /// copied out of `core/tests/fixtures/` by the build script — rather than
    /// a second copy that can drift.
    static func reviewFixture() throws -> OfflineTransport {
        let raw = try JSONSerialization.jsonObject(
            with: Data(try read("review_response").utf8)
        ) as! [String: Any]
        let data = raw["data"] as! [String: Any]
        let body: [String: Any] = [
            "data": [
                "requested": data["search"]!,
                "available": [
                    "issueCount": 0,
                    "pageInfo": ["hasNextPage": false, "endCursor": NSNull()],
                    "nodes": [],
                ],
                "rateLimit": data["rateLimit"]!,
            ]
        ]
        let encoded = try JSONSerialization.data(withJSONObject: body)
        return OfflineTransport(status: 200, body: String(data: encoded, encoding: .utf8)!)
    }

    private static func read(_ fixture: String) throws -> String {
        guard let url = Bundle.module.url(
            forResource: fixture, withExtension: "json", subdirectory: "Fixtures"
        ) else {
            throw XCTSkip("fixture \(fixture).json is not in the test bundle; run scripts/build-xcframework.sh")
        }
        return try String(contentsOf: url, encoding: .utf8)
    }

    init(status: UInt16, body: String) {
        self.body = body
        self.status = status
    }

    func send(request: GraphqlRequest) async throws -> HttpResponse {
        seen.append(request)
        return HttpResponse(
            status: status,
            headers: [Header(name: "x-ratelimit-remaining", value: "4999")],
            body: body
        )
    }
}

private final class FixedToken: TokenSource, @unchecked Sendable {
    weak var client: BoardClient?
    func token() async throws -> String { "ghu_offline" }
}

private func makeClient(_ transport: OfflineTransport) -> BoardClient {
    let tokens = FixedToken()
    let client = BoardClient(
        config: ClientConfig(
            host: "github.com",
            viewer: "me",
            userAgent: "PRMarmot-Smoke/0"
        ),
        transport: transport,
        tokens: tokens
    )
    transport.client = client
    tokens.client = client
    return client
}

/// Start from core's own defaults, never from a list retyped here. Getting
/// `bots` wrong changes categorization, which changes what counts as waiting,
/// which changes `is:stale` — this test found exactly that.
private func settings() -> BoardSettings {
    var settings = defaultBoardSettings()
    settings.staleAfterDays = 3
    return settings
}

final class SmokeTests: XCTestCase {
    func testABoardArrivesThroughASwiftTransport() async throws {
        let transport = try OfflineTransport(fixture: "authored_response")
        let client = makeClient(transport)

        let board = try await client.fetchBoard(
            mode: .authored,
            scope: .repository(name: "acme/widgets"),
            settings: settings(),
            nowEpoch: now
        )

        // The same 18 rows the Rust goldens count for this fixture.
        XCTAssertEqual(board.rows.count, 18)
        XCTAssertEqual(board.mode, .authored)
        XCTAssertNotNil(board.rateLimit)

        // Rows arrive derived: a Note, a category, a wait computed at `now`.
        XCTAssertFalse(board.rows[0].note.isEmpty)
        XCTAssertTrue(board.rows.contains { $0.stale })
        XCTAssertTrue(board.rows.contains { $0.waitLabel != nil })

        // One request, with the token and a real GraphQL body.
        XCTAssertEqual(transport.seen.count, 1)
        XCTAssertEqual(transport.seen[0].url, "https://api.github.com/graphql")
        XCTAssertEqual(transport.seen[0].token, "ghu_offline")
        XCTAssertEqual(transport.seen[0].userAgent, "PRMarmot-Smoke/0")
        XCTAssertTrue(transport.seen[0].body.contains("\"variables\""))
    }

    func testTheReviewQueueLaysOutIntoSections() async throws {
        let transport = try OfflineTransport.reviewFixture()
        let client = makeClient(transport)

        let board = try await client.fetchBoard(
            mode: .review,
            scope: .repository(name: "acme/widgets"),
            settings: settings(),
            nowEpoch: now
        )
        XCTAssertEqual(board.rows.count, 12)

        let items = layout(
            rows: board.rows,
            mode: .review,
            allRepos: false,
            snoozed: [],
            showSnoozed: false,
            sort: .wait
        )
        let headers = items.filter { if case .header = $0 { return true } else { return false } }
        let rows = items.filter { if case .row = $0 { return true } else { return false } }
        XCTAssertFalse(headers.isEmpty, "a review board has sections")
        XCTAssertEqual(rows.count, board.rows.count, "every row is placed exactly once")

        let payload = shareGroup(
            title: "Requested from you",
            rows: board.rows,
            mode: .review,
            format: .markdown
        )
        XCTAssertEqual(
            payload.plain.split(separator: "\n").first.map(String.init),
            "**Requested from you (12)** · acme/widgets"
        )
    }

    func testTheSearchGrammarIsTheOneFromCore() async throws {
        let transport = try OfflineTransport(fixture: "authored_response")
        let board = try await makeClient(transport).fetchBoard(
            mode: .authored,
            scope: .repository(name: "acme/widgets"),
            settings: settings(),
            nowEpoch: now
        )

        // The counts are the Rust goldens', for the same fixture and instant.
        func matching(_ query: String) throws -> [UInt32] {
            try search(rows: board.rows, query: query, nowEpoch: now, staleAfterDays: 3)
        }
        XCTAssertEqual(try matching("label:bug").count, 1)
        XCTAssertEqual(try matching("is:stale").count, 4)
        XCTAssertEqual(try matching("repo:widgets").count, 0)
        XCTAssertEqual(try matching("").count, 18)

        let chips = takeFilterChips(query: "label:\"help wanted\" spare", finished: true)
        XCTAssertEqual(chips.chips.count, 1)
        XCTAssertEqual(chips.chips[0].qualifier, .label)
        XCTAssertEqual(chips.chips[0].value, "help wanted")
        XCTAssertEqual(chips.chips[0].term, "label:\"help wanted\"")
        XCTAssertEqual(chips.rest, "spare")

        let added = withFilter(query: chips.rest, qualifier: .author, value: "alice")
        XCTAssertEqual(added, "spare author:alice")
        XCTAssertEqual(withFilter(query: added, qualifier: .author, value: "ALICE"), added)
    }

    func testTheAttentionStoreRoundTripsThroughBytes() async throws {
        let transport = try OfflineTransport(fixture: "authored_response")
        let board = try await makeClient(transport).fetchBoard(
            mode: .authored,
            scope: .repository(name: "acme/widgets"),
            settings: settings(),
            nowEpoch: now
        )

        let store = AttentionStore(host: "github.com", account: "me")
        for row in board.rows {
            let result = try store.observe(row: row)
            XCTAssertEqual(result.observed, .baseline)
            XCTAssertFalse(result.changed)
        }

        var moved = board.rows[0]
        moved.headOid = "deadbeef"
        let result = try store.observe(row: moved)
        XCTAssertEqual(result.observed, .changed(semantic: true))
        XCTAssertTrue(result.changed)

        let bytes = try store.toBytes()
        let restored = try AttentionStore.fromBytes(host: "github.com", account: "me", bytes: bytes)
        XCTAssertTrue(restored.isChanged(prId: moved.id))
        XCTAssertEqual(restored.count(), store.count())

        // Another account's state is refused, never merged.
        XCTAssertThrowsError(
            try AttentionStore.fromBytes(host: "github.com", account: "someone-else", bytes: bytes)
        )
    }

    func testGitHubSayingNoBecomesATypedSwiftError() async throws {
        let unauthorized = OfflineTransport(status: 401, body: "{\"message\":\"Bad credentials\"}")
        do {
            _ = try await makeClient(unauthorized).fetchBoard(
                mode: .authored,
                scope: .allRepositories,
                settings: settings(),
                nowEpoch: now
            )
            XCTFail("a 401 must not look like a board")
        } catch FfiError.NotAuthenticated {
            // The one the sign-in screen listens for.
        }

        let server = OfflineTransport(status: 502, body: "<html>502 Bad Gateway</html>")
        do {
            _ = try await makeClient(server).fetchBoard(
                mode: .authored,
                scope: .allRepositories,
                settings: settings(),
                nowEpoch: now
            )
            XCTFail("a 502 must not look like a board")
        } catch FfiError.Network(let message) {
            XCTAssertTrue(message.contains("having trouble (502)"), message)
        }
    }

    /// The cycle rule, from the Swift side: releasing the client must release
    /// everything, and the transport's back-reference must go `nil`.
    func testReleasingTheClientBreaksNoCycle() async throws {
        weak var weakClient: BoardClient?
        let transport = try OfflineTransport(fixture: "authored_response")

        // A nested scope rather than `autoreleasepool`, which takes a
        // synchronous closure: the strong reference has to die before the
        // assertions, and `client` going out of scope here is what kills it.
        func useAndRelease() async throws {
            let client = makeClient(transport)
            weakClient = client
            XCTAssertNotNil(transport.client)
            _ = try await client.fetchBoard(
                mode: .authored,
                scope: .allRepositories,
                settings: settings(),
                nowEpoch: now
            )
        }
        try await useAndRelease()

        XCTAssertNil(weakClient, "the client outlived its last strong reference")
        XCTAssertNil(transport.client, "the transport still points at a dead client")
    }

    func testTheLibraryReportsItsVersion() {
        XCTAssertFalse(coreVersion().isEmpty)
    }
}
