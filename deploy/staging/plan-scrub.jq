# Removes every secret from the JSON of `tofu show -json <planfile>` (code review F-5).
# remote.sh pipes the JSON through this filter, so the unscrubbed JSON never lands on disk:
#
#   tofu show -json plan.tfplan | jq -f plan-scrub.jq > plan.json
#
# After the first apply, a plan holds the prior state and the "before" values: the Talos machine
# secrets (cluster CA, etcd CA, bootstrap token), the talosconfig and the kubeconfig. This filter:
#   - removes .variables (the state passphrase), .prior_state and .configuration;
#   - replaces each value that a *_sensitive mask marks with "(sensitive)";
#   - replaces all values of the talos_* resources and of every data source with
#     {"redacted": true}, whether the provider marks them sensitive or not;
#   - replaces every output value with "(redacted)".
# check-plan.sh refuses a plan JSON that still holds .variables or .prior_state.

# mask(value; sensitive-mask): the value with every part that the mask marks true replaced.
def mask($v; $m):
  if $m == true then (if $v == null then null else "(sensitive)" end)
  elif ($m | type) == "object" and ($v | type) == "object" then
    reduce ($m | keys[]) as $k ($v; if has($k) then .[$k] = mask(.[$k]; $m[$k]) else . end)
  elif ($m | type) == "array" and ($v | type) == "array" then
    [ range(0; $v | length) as $i | mask($v[$i]; ($m[$i] // false)) ]
  else $v end;

def secret_type: (.type // "") | startswith("talos_");
def redact_resource: secret_type or ((.mode // "managed") == "data");

def scrub_module:
  (if has("resources") then
     .resources |= map(
       if redact_resource then .values = {"redacted": true}
       else .values = mask(.values; (.sensitive_values // {})) end)
   else . end)
  | (if has("child_modules") then .child_modules |= map(scrub_module) else . end);

del(.variables, .prior_state, .configuration)
| (if has("resource_changes") then
     .resource_changes |= map(
       if redact_resource then
         .change.before = (if .change.before == null then null else {"redacted": true} end)
         | .change.after = (if .change.after == null then null else {"redacted": true} end)
       else
         .change.before = mask(.change.before; (.change.before_sensitive // false))
         | .change.after = mask(.change.after; (.change.after_sensitive // false))
       end)
   else . end)
| (if has("resource_drift") then .resource_drift |= map(.change.before = null | .change.after = null) else . end)
| (if has("planned_values") and (.planned_values | has("root_module")) then .planned_values.root_module |= scrub_module else . end)
| (if has("planned_values") and (.planned_values | has("outputs")) then
     .planned_values.outputs |= map_values(.value = "(redacted)") else . end)
| (if has("output_changes") then
     .output_changes |= map_values(
       .before = (if .before == null then null else "(redacted)" end)
       | .after = (if .after == null then null else "(redacted)" end))
   else . end)
