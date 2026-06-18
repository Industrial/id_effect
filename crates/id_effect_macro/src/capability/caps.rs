//! `caps!` macro.

/// Required capability set for `Effect<_, _, caps!(…)>` (runtime: [`CapList`](::id_effect::CapList)).
#[macro_export]
macro_rules! caps {
  () => {
    ::id_effect::Env
  };
  ($k0:ty) => {
    ::id_effect::CapList<($k0,)>
  };
  ($k0:ty, $k1:ty) => {
    ::id_effect::CapList<($k0, $k1)>
  };
  ($k0:ty, $k1:ty, $k2:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6, $k7)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6, $k7, $k8)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6, $k7, $k8, $k9)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6, $k7, $k8, $k9, $k10)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6, $k7, $k8, $k9, $k10, $k11)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty, $k12:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6, $k7, $k8, $k9, $k10, $k11, $k12)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty, $k12:ty, $k13:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6, $k7, $k8, $k9, $k10, $k11, $k12, $k13)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty, $k12:ty, $k13:ty, $k14:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6, $k7, $k8, $k9, $k10, $k11, $k12, $k13, $k14)>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty, $k12:ty, $k13:ty, $k14:ty, $k15:ty) => {
    ::id_effect::CapList<($k0, $k1, $k2, $k3, $k4, $k5, $k6, $k7, $k8, $k9, $k10, $k11, $k12, $k13, $k14, $k15)>
  };
}
