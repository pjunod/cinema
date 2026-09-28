import XCTest
@testable import plurx

@MainActor
final class PlayerSeekCompletionTests: XCTestCase {
    private func race(finishFirst: Bool?, cancel: Bool = false) async throws {
        let operationEntered = expectation(description: "seek suspended")
        let deadlineEntered = expectation(description: "deadline suspended")
        let returned = expectation(description: "bounded wait returned")
        var finish: CheckedContinuation<Bool, Never>?
        var expire: CheckedContinuation<Void, Error>?
        let task = Task {
            let outcome = await PlayerSeekCompletion.run(operation: {
                await withCheckedContinuation {
                    finish = $0
                    operationEntered.fulfill()
                }
            }, waitForDeadline: {
                try await withCheckedThrowingContinuation {
                    expire = $0
                    deadlineEntered.fulfill()
                }
            })
            returned.fulfill()
            return outcome
        }
        await fulfillment(of: [operationEntered, deadlineEntered], timeout: 3)
        if cancel {
            task.cancel()
        } else if let finishFirst {
            try XCTUnwrap(finish).resume(returning: finishFirst)
            finish = nil
        } else {
            try XCTUnwrap(expire).resume()
            expire = nil
        }
        await fulfillment(of: [returned], timeout: 3)
        let outcome = await task.value
        XCTAssertEqual(outcome, cancel ? .cancelled : finishFirst.map { .finished($0) } ?? .timedOut)
        // Both adapters ignore cancellation and can call back late. Neither
        // may change the winning outcome or resume its continuation twice.
        finish?.resume(returning: true)
        expire?.resume()
        await Task.yield()
    }

    func testDeadlineReturnsBeforeANoncooperativeSeekCompletes() async throws {
        try await race(finishFirst: nil)
    }

    func testCancellationReturnsBeforeANoncooperativeSeekCompletes() async throws {
        try await race(finishFirst: nil, cancel: true)
    }

    func testCompletedSeekIgnoresALateDeadline() async throws {
        try await race(finishFirst: false)
    }
}
