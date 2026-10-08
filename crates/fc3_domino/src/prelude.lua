-- The Domino runtime around the game's scripts, as the engine sets it up
-- (our own implementation of its protocol; see docs/story.md).
--
-- A script file defines a box class and hands it over in the global
-- `export`. Loading it stores the class in `Boxes[GetPathID(path)]`, gives it
-- `_meta`/`_type` and runs its `Create(cbox)` once, which only declares
-- dependencies (other boxes, globals scripts, libraries, resources).
-- Instances are tables with the class's metatable whose `Init(cbox)` runs on
-- creation; stateless boxes are used as the class table itself. Engine calls
-- look methods up on the class (`inst._type[method]`). Natives the host
-- hasn't provided resolve to logging stand-ins.

Boxes = {}
Globals = {}
if PersistentGlobals == nil then
    PersistentGlobals = {}
end
DummyFunction = function() end

-- The graphs' debug tracing (only the debugger's twins call it).
function TraceConnection() end

System = {}
function System:Trace(...) __domino_log("trace", ...) end
function System:TraceLine(...) __domino_log("trace", ...) end
function System:Log(...) __domino_log("log", ...) end
function System:Warning(...) __domino_log("warning", ...) end
function System:SkipableWarning(...) __domino_log("warning", ...) end
function System:GetCurrentCallstack()
    return debug.traceback()
end

-- The resource side of the box context: what `Create` uses to declare
-- dependencies. Loading is immediate here, so a dependency is ready as soon
-- as it is declared.
local Resource = {}
Resource.__index = Resource

local resource = setmetatable({}, Resource)

local done = {}

-- Runs a globals script or a library once.
local function run_once(path)
    local id = GetPathID(path)
    if done[id] == nil then
        done[id] = true
        __domino_load(path)
    end
end

-- Loads a box class (once), as the engine's box resource does.
function __domino_class(path)
    local id = GetPathID(path)
    local class = Boxes[id]
    if class ~= nil then
        return class
    end
    rawset(_G, "export", nil)
    if not __domino_load(path) then
        -- A missing script: an empty class, so its users still load.
        class = {}
    else
        class = rawget(_G, "export")
        if type(class) ~= "table" then
            class = {}
        end
    end
    rawset(_G, "export", nil)
    rawset(_G, "_compilerVersion", nil)
    class._meta = { __index = class }
    class._type = class
    Boxes[id] = class
    if class.Create ~= nil then
        class.Create(class, resource)
    end
    if class.LuaDependencies ~= nil then
        for _, dep in ipairs(class.LuaDependencies(class) or {}) do
            __domino_class(dep)
        end
    end
    return class
end

function Resource:RegisterBox(path)
    __domino_class(path)
end

function Resource:RegisterGlobals(path)
    run_once(path)
end

function Resource:RegisterLibrary(path)
    run_once(path)
end

function Resource:LoadResource(name, kind)
    __domino_resource(name, kind)
end

function Resource:ShouldLoadResources()
    return true
end

function Resource:IsInEditorMode()
    return false
end

-- The instance side: one per graph running on an entity, shared by every
-- box the graph creates.
local Instance = {}
Instance.__index = Instance

function Instance:CreateBox(path)
    local class = __domino_class(path)
    local box = setmetatable({}, class._meta)
    box._type = class
    if class.Init ~= nil then
        class.Init(box, self)
    end
    return box
end

-- The entity whose Domino component runs the graph (the mission system's
-- "script owner").
function Instance:GetParentEntity()
    return self.parent
end

function Instance:GetParentPrefabEntityId()
    return self.prefab
end

-- Entry point for the host: a graph instance on an entity, its inputs
-- written before `Init`.
function __domino_create(path, parent, prefab, inputs)
    local class = __domino_class(path)
    local cbox = setmetatable({ parent = parent, prefab = prefab }, Instance)
    local box = setmetatable({}, class._meta)
    box._type = class
    if inputs ~= nil then
        for k, v in pairs(inputs) do
            box[k] = v
        end
    end
    if class.Init ~= nil then
        class.Init(box, cbox)
    end
    return box
end

-- Entry point for the host: an engine call into a box (looked up on its
-- class), as the engine makes them.
function __domino_call(box, method, ...)
    local class = box._type
    if class == nil or class[method] == nil then
        return
    end
    return class[method](box, ...)
end

-- Unknown natives: a stand-in that can be called (logging the call) or
-- indexed (giving a stand-in for the member), so a graph runs on before the
-- host provides them. Calls return nothing, but a manager singleton's
-- accessor returns a stand-in for the manager.
local stubs = {}
local function stub(name)
    local s = stubs[name]
    if s ~= nil then
        return s
    end
    s = setmetatable({}, {
        __index = function(_, member)
            return stub(name .. "." .. tostring(member))
        end,
        __call = function(_, ...)
            __domino_unknown(name, ...)
            if string.find(name, "GetInstance$") or string.find(name, "Manager$") then
                return stub(name .. "()")
            end
            return nil
        end,
    })
    stubs[name] = s
    return s
end
__domino_stub = stub

setmetatable(_G, {
    __index = function(_, name)
        if type(name) ~= "string" or not string.find(name, "^[A-Z]") then
            return nil
        end
        return stub(name)
    end,
})
