import { createContext, useContext, useEffect, useState, useSyncExternalStore } from "react";
import { createDashboardData } from "./dashboard.js";

const Data = createContext(null);
export const useData = () => useContext(Data);

export function Provider({ children }) {
  const [dashboard] = useState(() => createDashboardData());
  const state = useSyncExternalStore(dashboard.subscribe, dashboard.getSnapshot);
  useEffect(() => {
    dashboard.start();
    return () => dashboard.stop();
  }, [dashboard]);
  return (
    <Data.Provider value={{
      ...state,
      setProfile: dashboard.setProfile,
      refresh: dashboard.refresh,
      mutate: dashboard.mutate,
    }}>
      {children}
    </Data.Provider>
  );
}
