const name = "lookup";
const description = `Look up a record`;
const schema = { type: "object", properties: { key: { type: "string" } } };
server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: [
  { name, description, inputSchema: schema },
  { name: "ping", description: "Ping service", inputSchema: { type: "object" } },
  { name: "lookup", description: "Later description", inputSchema: {} },
] }));
const notATool = { name: "missing_schema", description: "Ordinary metadata" };
const notAToolEither = { inputSchema: {}, description: "No name" };
const dynamic = { name: getName(), inputSchema: {} };
