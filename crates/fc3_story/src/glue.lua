-- The engine's native objects, built over host functions registered under
-- "Class:Method" (singletons, called with the instance) and "Table.Function"
-- (API tables, called without one). Members the host doesn't provide fall
-- back to the logging stand-ins.

local function native_object(name, sep)
    return setmetatable({}, {
        __index = function(t, member)
            local f = rawget(_G, name .. sep .. member)
            if f == nil then
                return __domino_stub(name .. sep .. member)
            end
            if sep == ":" then
                local m = function(_, ...) return f(...) end
                rawset(t, member, m)
                return m
            end
            rawset(t, member, f)
            return f
        end,
    })
end

-- Singletons reached through `X_GetInstance()`.
for _, name in ipairs({
    "CScriptCallbackSystem", "CDominoDelayManager", "CDominoTickManager", "CGOStateMachineScriptCallbackSystem",
    "CDominoConsoleCommandManager", "CDominoWaterLevelManager", "CDominoSoundManager", "CEntityGroupManager",
    "CFCXDominoUiManager", "CSequenceManager",
}) do
    local object = native_object(name, ":")
    rawset(_G, name .. "_GetInstance", function() return object end)
end
GetSequenceManager = CSequenceManager_GetInstance

-- Singletons reached through `X.GetInstance()`.
for _, name in ipairs({
    "CMissionManager", "CFCXHudDisplayHelper", "CFCXMusicService", "CMissionBlockManager",
    "CRecastNavmeshDynamicManager", "CFCXAchievementService", "CStatisticServiceSP",
}) do
    local object = native_object(name, ":")
    rawset(_G, name, { GetInstance = function() return object end })
end

-- API tables.
for _, name in ipairs({
    "CAPI_Entity", "CAPI_Pawn", "CAPI_Player", "CAPI_Vehicle", "CAPI_NPC", "CAPI_Companion", "CAPI_AI",
    "CAPI_Cinema", "CAPI_Weapon", "CAPI_Trigger", "CAPI_Animal", "CAPI_Sound",
}) do
    rawset(_G, name, native_object(name, "."))
end
