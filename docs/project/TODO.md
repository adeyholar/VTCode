Audit the codebase for oversized files, monolithic modules, duplicated logic, and tightly coupled components. Identify refactoring opportunities, extract reusable components, and improve modularity and separation of concerns. Prioritize by complexity, duplication, maintainability, and impact. Produce a phased refactoring plan with file references, proposed module boundaries, and risks. Follow KISS/DRY, preserve existing behavior and public APIs, and avoid unnecessary abstractions.

===

fix compaction error:

```

 Model switched, but context compaction failed: Failed to compact history via
 provider-native compaction: Provider error: OpenAI Compaction endpoint error (status
 400 Bad Request) [request_id=req_58499769276f45d69f49423f4595599d client_request_id=
 vtcode-18f64c1f-b3c0-4a40-aad8-12b30224f57b code=unknown_parameter type=
 invalid_request_error param=stream organization=user-3bdwrazmtbbupklnw8cnitng]
 Message: Unknown parameter: 'stream'. Body: {
   "error": {
     "message": "Unknown parameter: 'stream'.",
     "type": "invalid_request_error",
     "param": "stream",
     "code": "unknown_parameter"
   }
 }. Continuing with full history plus auto-resume note.
```

--


gui


--

cua
