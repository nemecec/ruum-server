# Normalize SelfSubjectRulesReview JSON or the four-column kubectl table. Never
# discard an unknown line (including incomplete-review warnings).
def words: if . == "" then [] else split(" ") | map(select(length > 0)) end;
def strings: type == "array" and all(.[]; type == "string");
def valid:
  (.verbs | strings and length > 0) and
  (.resourceNames | strings) and (.apiGroups | strings) and
  (.resources | strings) and (.nonResourceURLs | strings) and
  (((.resources|length)>0 and (.apiGroups|length)>0 and (.nonResourceURLs|length)==0) or
   ((.resources|length)==0 and (.nonResourceURLs|length)>0));
(if test("^\\s*\\{") then
  fromjson | .status |
  if .incomplete == false and ((.evaluationError // "") == "") and
    (.resourceRules|type)=="array" and (.nonResourceRules|type)=="array"
  then .resourceRules + .nonResourceRules else error("incomplete rules review") end
else
  split("\n") | map(select(length > 0)) |
  if (.[0] | test("^Resources\\s+Non-Resource URLs\\s+Resource Names\\s+Verbs\\s*$")) then
    # Slice at header offsets: URL rules have an empty first column, and lists
    # contain spaces. Preserve duplicate rows so every grant is audited.
    (.[0] | [index("Non-Resource URLs"), index("Resource Names"), index("Verbs")]) as $starts |
    def trim: gsub("^ +| +$"; "");
    def list:
      trim | if test("^\\[[^][]*\\]$") then .[1:-1] | words
      else error("unreadable rule list") end;
    .[1:] | map(
      . as $row |
      ($row[:$starts[0]] | trim) as $resource |
      if ($resource | test("^\\S*$")) then
        ($resource | split(".")) as $parts |
        {apiGroups:(if $resource=="*.*" then ["*"] elif $resource=="" then [] else [$parts[1:]|join(".")] end),
         resources:(if $resource=="" then [] else [$parts[0]] end),
         nonResourceURLs:($row[$starts[0]:$starts[1]] | list),
         resourceNames:($row[$starts[1]:$starts[2]] | list),
         verbs:($row[$starts[2]:] | list)}
      else error("unreadable rule row") end)
  else error("unreadable rule header") end
end) | map(. + {resourceNames:(.resourceNames // []), apiGroups:(.apiGroups // []),
  resources:(.resources // []), nonResourceURLs:(.nonResourceURLs // [])}) |
if all(.[]; valid) then . else error("invalid rule") end
