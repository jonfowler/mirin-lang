module drop_all (
    input  logic [7:0] v [0:3],
    output logic [7:0] result [0:0-1]
);
    assign result = '{default: '0};
endmodule

module head #(parameter int k) (
    input  logic [7:0] v [0:3],
    output logic [7:0] result [0:k-1]
);
    logic [7:0] __block_0 [0:(k - 0)-1];
    if (((k - 0) == 0)) begin : __block_0__g
        assign __block_0 = '{default: '0};
    end else begin : __block_0__g
        assign __block_0 = v[0 +: (k - 0)];
    end
    assign result = __block_0;
endmodule

module use_zero (
    input  logic [7:0] v [0:3],
    output logic [7:0] result [0:0-1]
);
    head #(
        .k(0)
    ) head (
        .v(v),
        .result(result)
    );
endmodule

module use_two (
    input  logic [7:0] v [0:3],
    output logic [7:0] result [0:1]
);
    head #(
        .k(2)
    ) head (
        .v(v),
        .result(result)
    );
endmodule
