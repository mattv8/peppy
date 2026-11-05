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
	"FinishGaiaPairing": true, "Connect": true, "Disconnect": true, "ListConversations": true, "GetConversation": true,
	"GetOrCreateConversation": true, "SendMessage": true, "UnpairGaia": true,
}

// forbiddenRPCNames are the read, send, open, typing, delete and Bugle-revoke methods.
var forbiddenRPCNames = []string{
	"MarkRead", "SetTyping", "SendReaction", "DeleteMessage",
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
	walkNodes(files, func(node ast.Node) {
		call, ok := node.(*ast.CallExpr)
		if !ok {
			return
		}
		switch calleeName(call) {
		case "emit":
			kind, okKind := literalArg(call, 0)
			code, okCode := literalArg(call, 1)
			if okKind && okCode {
				check(kind, code)
			}
		case "transition", "stopFatal", "pairFailure":
			// These emit on the link kind (transition takes state first, then code).
			index := 0
			if calleeName(call) == "transition" {
				index = 1
			}
			if code, ok := literalArg(call, index); ok {
				check("link", code)
			}
		}
	})
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
	walkNodes(files, func(node ast.Node) {
		call, ok := node.(*ast.CallExpr)
		if !ok {
			return
		}
		switch calleeName(call) {
		case "New":
			if code, ok := literalArg(call, 0); ok {
				check(code)
			}
		case "pairFailure", "stopFatal", "signOut", "signOutIf":
			if code, ok := literalArg(call, 0); ok {
				check(code)
			}
		case "transition":
			if code, ok := literalArg(call, 1); ok {
				check(code)
			}
		}
	})
	for _, name := range []string{"pairCode", "transportStop"} {
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

// TestNoReadOrSendRPCs is an allowlist: the only libgm client methods the link may
// call are listed, and the read, send, open, typing, delete and Bugle-revoke names are
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
