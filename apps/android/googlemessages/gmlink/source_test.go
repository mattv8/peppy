package gmlink

import (
	"go/ast"
	"go/parser"
	"go/token"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
)

// fixedCodePattern is the shape every error and stop code must have (lesson 6).
var fixedCodePattern = regexp.MustCompile(`^[a-z][a-z0-9_]{0,39}$`)

// allowedLibgmCalls are the only libgm client methods the link may call.
var allowedLibgmCalls = map[string]bool{
	"FetchConfig": true, "SetEventHandler": true, "SetPingInterval": true, "StartGaiaPairing": true,
	"SetRPCDiagnosticObserver": true, // Local fixed-enum diagnostics configuration; no RPC effect.
	"FinishGaiaPairing":        true, "Connect": true, "Disconnect": true, "ListConversations": true, "GetConversation": true,
	"GetOrCreateConversation": true, "SendMessage": true, "FetchMessages": true, "SetTyping": true, "UnpairGaia": true,
}

// forbiddenRPCNames are the unapproved read, send, open, delete and Bugle-revoke methods.
var forbiddenRPCNames = []string{
	"MarkRead", "SendReaction", "DeleteMessage",
	"UpdateConversation", "UpdateSettings", "UnpairBugle", "Unpair", "RevokeRelayPairing",
}

// parseSources parses the package's non-test sources. The scans read the code
// itself, so a new literal or call is checked without a list to update.
func parseSources(t *testing.T) []*ast.File {
	t.Helper()
	paths, err := filepath.Glob("*.go")
	if err != nil {
		t.Fatal(err)
	}
	fset := token.NewFileSet()
	var files []*ast.File
	for _, path := range paths {
		if strings.HasSuffix(path, "_test.go") {
			continue
		}
		file, err := parser.ParseFile(fset, path, nil, 0)
		if err != nil {
			t.Fatal(err)
		}
		files = append(files, file)
	}
	return files
}

func walkNodes(files []*ast.File, visit func(node ast.Node)) {
	for _, file := range files {
		ast.Inspect(file, func(node ast.Node) bool {
			visit(node)
			return true
		})
	}
}

// calleeName is the function or method name a call invokes.
func calleeName(call *ast.CallExpr) string {
	switch fn := call.Fun.(type) {
	case *ast.Ident:
		return fn.Name
	case *ast.SelectorExpr:
		return fn.Sel.Name
	}
	return ""
}

// literalArg returns the string literal at index, if that argument is one.
func literalArg(call *ast.CallExpr, index int) (string, bool) {
	if index >= len(call.Args) {
		return "", false
	}
	lit, ok := call.Args[index].(*ast.BasicLit)
	if !ok || lit.Kind != token.STRING {
		return "", false
	}
	value, err := strconv.Unquote(lit.Value)
	return value, err == nil
}

// funcBody returns the body of the named top-level function.
func funcBody(t *testing.T, files []*ast.File, name string) *ast.BlockStmt {
	t.Helper()
	for _, file := range files {
		for _, decl := range file.Decls {
			if fn, ok := decl.(*ast.FuncDecl); ok && fn.Name.Name == name {
				return fn.Body
			}
		}
	}
	t.Fatalf("function %s not found in the sources", name)
	return nil
}

func emittedCallCodes(files []*ast.File) []eventCode {
	var codes []eventCode
	walkNodes(files, func(node ast.Node) {
		call, ok := node.(*ast.CallExpr)
		if !ok {
			return
		}
		add := func(kindIndex, codeIndex int) {
			kind, okKind := literalArg(call, kindIndex)
			code, okCode := literalArg(call, codeIndex)
			if okKind && okCode {
				codes = append(codes, eventCode{kind, code})
			}
		}
		switch calleeName(call) {
		case "emit":
			add(0, 1)
		case "emitFor":
			add(1, 2)
		case "transitionFor":
			if code, ok := literalArg(call, 2); ok {
				codes = append(codes, eventCode{"link", code})
			}
		case "stopFatalFor", "signOutIfFor":
			if code, ok := literalArg(call, 1); ok {
				codes = append(codes, eventCode{"link", code})
			}
		case "pairFailure":
			if code, ok := literalArg(call, 0); ok {
				codes = append(codes, eventCode{"link", code})
			}
		}
	})
	return codes
}

func fixedCallCodes(files []*ast.File) []string {
	var codes []string
	walkNodes(files, func(node ast.Node) {
		call, ok := node.(*ast.CallExpr)
		if !ok {
			return
		}
		index := -1
		switch calleeName(call) {
		case "New", "pairFailure", "signOut":
			index = 0
		case "stopFatalFor", "signOutIfFor":
			index = 1
		case "transitionFor":
			index = 2
		}
		if index >= 0 {
			if code, ok := literalArg(call, index); ok {
				codes = append(codes, code)
			}
		}
	})
	return codes
}

// TestEveryEmittedCodeIsListed checks every literal event code the sources can emit
// against the shared list in events.go.
func TestEveryEmittedCodeIsListed(t *testing.T) {
	files := parseSources(t)
	listed := map[eventCode]bool{}
	for _, code := range eventCodes {
		listed[code] = true
	}
	checked := 0
	check := func(kind, code string) {
		checked++
		if !listed[eventCode{kind, code}] {
			t.Errorf("emitted %s/%s is not in the shared event list", kind, code)
		}
	}
	for _, code := range emittedCallCodes(files) {
		check(code.kind, code.code)
	}
	// The pair's deferred restore emits whatever pairCode returns.
	ast.Inspect(funcBody(t, files, "pairCode"), func(node ast.Node) bool {
		if ret, ok := node.(*ast.ReturnStmt); ok {
			for _, result := range ret.Results {
				if lit, ok := result.(*ast.BasicLit); ok && lit.Kind == token.STRING {
					if value, err := strconv.Unquote(lit.Value); err == nil {
						check("link", value)
					}
				}
			}
		}
		return true
	})
	if checked < 10 {
		t.Fatalf("only %d emitted codes found; the scan is not reading the sources", checked)
	}
}

// TestEveryReturnedErrorIsFixedCode checks every literal error text and stop code
// the sources can produce against the fixed-code shape.
func TestEveryReturnedErrorIsFixedCode(t *testing.T) {
	files := parseSources(t)
	checked := 0
	check := func(code string) {
		checked++
		if !fixedCodePattern.MatchString(code) {
			t.Errorf("%q is not a fixed code", code)
		}
	}
	for _, code := range fixedCallCodes(files) {
		check(code)
	}
	for _, name := range []string{"pairCode", "transportStopFor"} {
		ast.Inspect(funcBody(t, files, name), func(node ast.Node) bool {
			var values []ast.Expr
			switch clause := node.(type) {
			case *ast.ReturnStmt:
				values = clause.Results
			case *ast.CaseClause:
				values = clause.List
			}
			for _, value := range values {
				if lit, ok := value.(*ast.BasicLit); ok && lit.Kind == token.STRING {
					if text, err := strconv.Unquote(lit.Value); err == nil {
						check(text)
					}
				}
			}
			return true
		})
	}
	if checked < 10 {
		t.Fatalf("only %d error and stop codes found; the scan is not reading the sources", checked)
	}
}

func TestCodeLiteralArgumentPositionsRejectInvalidLiterals(t *testing.T) {
	tests := []struct {
		name          string
		source        string
		kind          string
		code          string
		emittedListed bool
		fixed         bool
		fixedValid    bool
	}{
		{"emitFor valid", `func f() { link.emitFor(1, "conn", "ready", 0) }`, "conn", "ready", true, false, false},
		{"emitFor unlisted", `func f() { link.emitFor(1, "conn", "unlisted_code", 0) }`, "conn", "unlisted_code", false, false, false},
		{"transitionFor valid", `func f() { link.transitionFor(1, "state", "relink_needed") }`, "link", "relink_needed", true, true, true},
		{"transitionFor unlisted", `func f() { link.transitionFor(1, "state", "unlisted_code") }`, "link", "unlisted_code", false, true, true},
		{"transitionFor malformed", `func f() { link.transitionFor(1, "state", "Invalid-Code") }`, "link", "Invalid-Code", false, true, false},
		{"stopFatalFor valid", `func f() { link.stopFatalFor(1, "persist_failed", false) }`, "link", "persist_failed", true, true, true},
		{"stopFatalFor unlisted", `func f() { link.stopFatalFor(1, "unlisted_code", false) }`, "link", "unlisted_code", false, true, true},
		{"stopFatalFor malformed", `func f() { link.stopFatalFor(1, "Invalid-Code", false) }`, "link", "Invalid-Code", false, true, false},
		{"signOutIfFor valid", `func f() { link.signOutIfFor(1, "signed_out", allowed) }`, "link", "signed_out", true, true, true},
		{"signOutIfFor unlisted", `func f() { link.signOutIfFor(1, "unlisted_code", allowed) }`, "link", "unlisted_code", false, true, true},
		{"signOutIfFor malformed", `func f() { link.signOutIfFor(1, "Invalid-Code", allowed) }`, "link", "Invalid-Code", false, true, false},
		{"pairFailure valid", `func f() { _ = pairFailure("pair_failed") }`, "link", "pair_failed", true, true, true},
		{"pairFailure unlisted", `func f() { _ = pairFailure("unlisted_code") }`, "link", "unlisted_code", false, true, true},
		{"pairFailure malformed", `func f() { _ = pairFailure("Invalid-Code") }`, "link", "Invalid-Code", false, true, false},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			file, err := parser.ParseFile(token.NewFileSet(), "synthetic.go", "package gmlink\n"+test.source, 0)
			if err != nil {
				t.Fatal(err)
			}
			emitted := emittedCallCodes([]*ast.File{file})
			if len(emitted) != 1 || emitted[0] != (eventCode{test.kind, test.code}) {
				t.Fatalf("emitted scanner codes = %#v, want %#v", emitted, []eventCode{{test.kind, test.code}})
			}
			if isEventCode(emitted[0].kind, emitted[0].code) != test.emittedListed {
				t.Fatalf("emitted scanner listed = %t, want %t", isEventCode(emitted[0].kind, emitted[0].code), test.emittedListed)
			}
			if test.fixed {
				fixed := fixedCallCodes([]*ast.File{file})
				if len(fixed) != 1 || fixed[0] != test.code {
					t.Fatalf("fixed-code scanner codes = %#v, want %#v", fixed, []string{test.code})
				}
				if fixedCodePattern.MatchString(fixed[0]) != test.fixedValid {
					t.Fatalf("fixed-code scanner valid = %t, want %t", fixedCodePattern.MatchString(fixed[0]), test.fixedValid)
				}
			}
		})
	}
}

// TestNoReadOrSendRPCs is an allowlist: the only libgm client methods the link may
// call are listed, and unapproved read, send, open, delete and Bugle-revoke names are
// absent from the sources.
func TestNoReadOrSendRPCs(t *testing.T) {
	files := parseSources(t)
	called := map[string]bool{}
	walkNodes(files, func(node ast.Node) {
		if call, ok := node.(*ast.CallExpr); ok {
			if selector, ok := call.Fun.(*ast.SelectorExpr); ok && isClientValue(selector.X) {
				called[selector.Sel.Name] = true
				if !allowedLibgmCalls[selector.Sel.Name] {
					t.Errorf("libgm client call %s is not on the allowlist", selector.Sel.Name)
				}
			}
		}
		if selector, ok := node.(*ast.SelectorExpr); ok {
			for _, forbidden := range forbiddenRPCNames {
				if selector.Sel.Name == forbidden {
					t.Errorf("forbidden RPC reference %s", forbidden)
				}
			}
		}
	})
	for _, required := range []string{"FetchConfig", "Connect", "ListConversations", "UnpairGaia", "StartGaiaPairing", "FinishGaiaPairing"} {
		if !called[required] {
			t.Fatalf("the scan did not see the expected call %s; it is not reading the client calls", required)
		}
	}
}

// isClientValue reports whether an expression is the libgm client the link holds:
// a local named client or a field named client.
func isClientValue(expr ast.Expr) bool {
	switch value := expr.(type) {
	case *ast.Ident:
		return value.Name == "client"
	case *ast.SelectorExpr:
		return value.Sel.Name == "client"
	}
	return false
}
