import ts from "typescript";
import { describe, expect, it } from "vitest";

const UNSAFE_HTML_PROPERTIES = new Set(["innerHTML", "outerHTML"]);
const UNSAFE_DOCUMENT_METHODS = new Set(["write", "writeln"]);
const SOURCE_ROOTS = ["apps/web/src", "apps/desktop/src", "packages/desktop-ui/src", "packages/browser-runtime/src"];

type Violation = {
  file: string;
  line: number;
  sink: string;
};

function propertyName(node: ts.PropertyAccessExpression | ts.ElementAccessExpression): string | undefined {
  if (ts.isPropertyAccessExpression(node)) {
    return node.name.text;
  }
  return ts.isStringLiteral(node.argumentExpression) ? node.argumentExpression.text : undefined;
}

function isDocumentExpression(node: ts.Expression): boolean {
  if (ts.isIdentifier(node)) {
    return node.text === "document";
  }
  if (ts.isPropertyAccessExpression(node)) {
    return node.name.text === "document";
  }
  return ts.isElementAccessExpression(node) && ts.isStringLiteral(node.argumentExpression) && node.argumentExpression.text === "document";
}

function addViolation(violations: Violation[], sourceFile: ts.SourceFile, node: ts.Node, sink: string): void {
  violations.push({
    file: sourceFile.fileName,
    line: sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1,
    sink,
  });
}

function isAssignmentOperator(kind: ts.SyntaxKind): boolean {
  return kind >= ts.SyntaxKind.FirstAssignment && kind <= ts.SyntaxKind.LastAssignment;
}

function findUnsafeHtmlWrites(source: string, fileName: string): Violation[] {
  const sourceFile = ts.createSourceFile(fileName, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const violations: Violation[] = [];

  const visit = (node: ts.Node): void => {
    if (ts.isBinaryExpression(node) && isAssignmentOperator(node.operatorToken.kind) && (ts.isPropertyAccessExpression(node.left) || ts.isElementAccessExpression(node.left))) {
      const name = propertyName(node.left);
      if (name && UNSAFE_HTML_PROPERTIES.has(name)) {
        addViolation(violations, sourceFile, node.left, name);
      }
    }

    if (ts.isCallExpression(node) && (ts.isPropertyAccessExpression(node.expression) || ts.isElementAccessExpression(node.expression))) {
      const name = propertyName(node.expression);
      if (name === "insertAdjacentHTML") {
        addViolation(violations, sourceFile, node.expression, name);
      }
      if (name && UNSAFE_DOCUMENT_METHODS.has(name) && isDocumentExpression(node.expression.expression)) {
        addViolation(violations, sourceFile, node.expression, `document.${name}`);
      }
    }

    if (ts.isJsxAttribute(node) && ts.isIdentifier(node.name) && node.name.text === "dangerouslySetInnerHTML") {
      addViolation(violations, sourceFile, node.name, node.name.text);
    }

    if (ts.isPropertyAssignment(node) && ts.isIdentifier(node.name) && node.name.text === "dangerouslySetInnerHTML") {
      addViolation(violations, sourceFile, node.name, node.name.text);
    }

    ts.forEachChild(node, visit);
  };

  visit(sourceFile);
  return violations;
}

function repositoryRoot(): string {
  let directory = ts.sys.getCurrentDirectory();
  while (!ts.sys.fileExists(`${directory}/pnpm-workspace.yaml`)) {
    const parent = directory.slice(0, Math.max(directory.lastIndexOf("/"), directory.lastIndexOf("\\")));
    if (parent === directory) {
      throw new Error("Could not locate repository root");
    }
    directory = parent;
  }
  return directory;
}

function isProductionSourceFile(fileName: string): boolean {
  const segments = fileName.split(/[\\/]/);
  const baseName = segments.at(-1) ?? "";
  return !/\.(test|spec)\.tsx?$/.test(baseName) && !segments.some(segment => ["fixtures", "__fixtures__", "generated", "build", "node_modules"].includes(segment));
}

describe("unsafe HTML policy", () => {
  it("detects direct and computed unsafe sinks without rejecting reads or textContent", () => {
    const source = `
      element.innerHTML;
      element.textContent = value;
      element.innerHTML = value;
      element["outerHTML"] = value;
      element["insertAdjacentHTML"]("beforeend", value);
      document["write"](value);
      document.writeln(value);
      <div dangerouslySetInnerHTML={{ __html: value }} />;
      createElement("div", { dangerouslySetInnerHTML: { __html: value } });
    `;

    expect(findUnsafeHtmlWrites(source, "synthetic.tsx").map(violation => violation.sink)).toEqual([
      "innerHTML",
      "outerHTML",
      "insertAdjacentHTML",
      "document.write",
      "document.writeln",
      "dangerouslySetInnerHTML",
      "dangerouslySetInnerHTML",
    ]);
  });

  it("keeps production TypeScript free of unsafe HTML sinks", () => {
    const root = repositoryRoot();
    const violations = SOURCE_ROOTS.flatMap(sourceRoot =>
      ts.sys
        .readDirectory(`${root}/${sourceRoot}`, [".ts", ".tsx"])
        .filter(isProductionSourceFile)
        .flatMap(fileName => findUnsafeHtmlWrites(ts.sys.readFile(fileName) ?? "", fileName.slice(root.length + 1))),
    );

    expect(violations).toEqual([]);
  });
});
