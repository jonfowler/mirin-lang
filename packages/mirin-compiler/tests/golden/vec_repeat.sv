module rep #(parameter int n) (
    input  logic [7:0] x,
    output logic [7:0] result
);
    logic [7:0] v [0:n-1];
    assign v = '{n{x}};
    assign result = v[0];
endmodule
