server.addTool("echo", cb);
// Repeated local names mirror separate registration modules in npm packages.
function echo() {
  const name = "echo";
  const description = `Echo text`;
  const schema = z.object({ text: z.string() });
  const config = { description, inputSchema: schema };
  server.registerTool(name, config, cb);
}
function add() {
  const name = "add";
  const description = "Add numbers";
  const schema = { a: z.number(), b: z.number() };
  server.tool(name, description, schema, cb);
}
server.addTool("echo", cb);
server.registerTool("echo", { description: "Later echo", inputSchema: {} }, cb);
// These cannot be resolved under the static extraction contract.
let mutable = "mutable";
const computed = "com" + "puted";
const alias = computed;
const substituted = `dynamic_${value}`;
server.tool(mutable, cb);
server.tool(computed, cb);
server.tool(alias, cb);
server.tool(substituted, cb);
server.tool(importedName, cb);
const shadowed = "outer";
function parameter(shadowed) { server.tool(shadowed, cb); }
function local() { let shadowed = "inner"; server.tool(shadowed, cb); }
function functionShadow() {
  function shadowed() {}
  server.tool(shadowed, cb);
}
function varShadow() {
  { var shadowed = "mutable"; }
  server.tool(shadowed, cb);
}
