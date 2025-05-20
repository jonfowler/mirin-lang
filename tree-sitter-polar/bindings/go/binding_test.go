package tree_sitter_polar_test

import (
	"testing"

	tree_sitter "github.com/tree-sitter/go-tree-sitter"
	tree_sitter_polar "github.com/tree-sitter/tree-sitter-polar/bindings/go"
)

func TestCanLoadGrammar(t *testing.T) {
	language := tree_sitter.NewLanguage(tree_sitter_polar.Language())
	if language == nil {
		t.Errorf("Error loading Polar grammar")
	}
}
