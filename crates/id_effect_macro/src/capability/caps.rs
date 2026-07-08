//! `caps!` macro.

/// Required capability set for `Effect<_, _, caps!(…)>` (runtime: [`CapList`](::id_effect::CapList)).
#[macro_export]
macro_rules! caps {
  () => {
    ::id_effect::Env
  };
  ($k0:ty) => {
    ::id_effect::CapList<(::id_effect::Cap<$k0>,)>
  };
  ($k0:ty, $k1:ty) => {
    ::id_effect::CapList<(::id_effect::Cap<$k0>, ::id_effect::Cap<$k1>)>
  };
  ($k0:ty, $k1:ty, $k2:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
      ::id_effect::Cap<$k7>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
      ::id_effect::Cap<$k7>,
      ::id_effect::Cap<$k8>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
      ::id_effect::Cap<$k7>,
      ::id_effect::Cap<$k8>,
      ::id_effect::Cap<$k9>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
      ::id_effect::Cap<$k7>,
      ::id_effect::Cap<$k8>,
      ::id_effect::Cap<$k9>,
      ::id_effect::Cap<$k10>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
      ::id_effect::Cap<$k7>,
      ::id_effect::Cap<$k8>,
      ::id_effect::Cap<$k9>,
      ::id_effect::Cap<$k10>,
      ::id_effect::Cap<$k11>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty, $k12:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
      ::id_effect::Cap<$k7>,
      ::id_effect::Cap<$k8>,
      ::id_effect::Cap<$k9>,
      ::id_effect::Cap<$k10>,
      ::id_effect::Cap<$k11>,
      ::id_effect::Cap<$k12>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty, $k12:ty, $k13:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
      ::id_effect::Cap<$k7>,
      ::id_effect::Cap<$k8>,
      ::id_effect::Cap<$k9>,
      ::id_effect::Cap<$k10>,
      ::id_effect::Cap<$k11>,
      ::id_effect::Cap<$k12>,
      ::id_effect::Cap<$k13>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty, $k12:ty, $k13:ty, $k14:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
      ::id_effect::Cap<$k7>,
      ::id_effect::Cap<$k8>,
      ::id_effect::Cap<$k9>,
      ::id_effect::Cap<$k10>,
      ::id_effect::Cap<$k11>,
      ::id_effect::Cap<$k12>,
      ::id_effect::Cap<$k13>,
      ::id_effect::Cap<$k14>,
    )>
  };
  ($k0:ty, $k1:ty, $k2:ty, $k3:ty, $k4:ty, $k5:ty, $k6:ty, $k7:ty, $k8:ty, $k9:ty, $k10:ty, $k11:ty, $k12:ty, $k13:ty, $k14:ty, $k15:ty) => {
    ::id_effect::CapList<(
      ::id_effect::Cap<$k0>,
      ::id_effect::Cap<$k1>,
      ::id_effect::Cap<$k2>,
      ::id_effect::Cap<$k3>,
      ::id_effect::Cap<$k4>,
      ::id_effect::Cap<$k5>,
      ::id_effect::Cap<$k6>,
      ::id_effect::Cap<$k7>,
      ::id_effect::Cap<$k8>,
      ::id_effect::Cap<$k9>,
      ::id_effect::Cap<$k10>,
      ::id_effect::Cap<$k11>,
      ::id_effect::Cap<$k12>,
      ::id_effect::Cap<$k13>,
      ::id_effect::Cap<$k14>,
      ::id_effect::Cap<$k15>,
    )>
  };
}
