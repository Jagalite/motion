# Development tool: regenerate the JSON form of the reviewed API contract.
#   ruby scripts/contract_json.rb [--check]
# The server serves the JSON at /api/v2/openapi.json; YAML remains the source.
# --check compares parsed documents, so it is independent of Ruby's JSON
# formatting (the older Ruby bundled with macOS formats differently).
require "yaml"
require "json"

root = File.expand_path("..", __dir__)
yaml = File.join(root, "contracts/Motion_Server_API_v2.yaml")
json = File.join(root, "contracts/Motion_Server_API_v2.json")
document = YAML.safe_load(File.read(yaml))
if ARGV.include?("--check")
  abort "#{json} differs from the YAML; run ruby scripts/contract_json.rb" unless JSON.parse(File.read(json)) == document
else
  File.write(json, JSON.pretty_generate(document) + "\n")
end
