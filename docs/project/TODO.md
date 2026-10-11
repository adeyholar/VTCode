Audit the codebase for oversized files, monolithic modules, duplicated logic, and tightly coupled components. Identify refactoring opportunities, extract reusable components, and improve modularity and separation of concerns. Prioritize by complexity, duplication, maintainability, and impact. Produce a phased refactoring plan with file references, proposed module boundaries, and risks. Follow KISS/DRY, preserve existing behavior and public APIs, and avoid unnecessary abstractions.

===

improve

<webmcp_authoritative_diff>

```diff
--- a/docs/project/TODO.md
+++ b/docs/project/TODO.md
@@ -1 +1,3 @@
+hello
+
Audit the codebase for oversized files, monolithic
modules, duplicated logic, and tightly coupled components
. Identify refactoring opportunities, extract reusable
components, and improve modularity and separation of
concerns. Prioritize by complexity, duplication,
maintainability, and impact. Produce a phased refactoring
plan with file references, proposed module boundaries,
and risks. Follow KISS/DRY, preserve existing behavior
and public APIs, and avoid unnecessary abstractions.
```

</webmcp_authoritative_diff>

diff rendering in VT COde for WebMCP

screenshot: '/Users/vinhnguyenxuan/Desktop/Screenshot 2026-10-11 at 14.44.11.png'
