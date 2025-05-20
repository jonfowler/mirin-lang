import XCTest
import SwiftTreeSitter
import TreeSitterPolar

final class TreeSitterPolarTests: XCTestCase {
    func testCanLoadGrammar() throws {
        let parser = Parser()
        let language = Language(language: tree_sitter_polar())
        XCTAssertNoThrow(try parser.setLanguage(language),
                         "Error loading Polar grammar")
    }
}
